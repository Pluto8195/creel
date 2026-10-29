//! Per-project agent accounts (managed accounts): the UI-side store of the
//! latest `ManagedAccountsSnapshot`, pure lookup helpers, and the shared
//! [`AccountFlow`] entity (project account picker + add-account sign-in) used
//! by the sidebar Space menu and Settings > Accounts.
//!
//! Accounts are device-local, so snapshots are keyed by the owning device id
//! and every call targets the device that owns the project.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, Focusable, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};
use mast_proto::{
    AgentLoginPoll, AgentLoginStart, AgentLoginStatus, HarnessId, ManagedAccount,
    ManagedAccountsSnapshot,
};
use mast_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover;
use crate::state::AppState;
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Only these harnesses support managed accounts.
pub fn supports_accounts(harness: HarnessId) -> bool {
    matches!(harness, HarnessId::ClaudeCode | HarnessId::Codex)
}

pub fn harness_name(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::Codex => "Codex",
        _ => "Claude",
    }
}

/// The managed account a project defaults to for `harness`; `None` = Default
/// (no entry, an explicit null, or a dangling id).
pub fn project_account<'a>(
    snapshot: &'a ManagedAccountsSnapshot,
    space_id: &str,
    harness: HarnessId,
) -> Option<&'a ManagedAccount> {
    let id = snapshot
        .project_defaults
        .iter()
        .find(|d| d.space_id == space_id && d.harness == harness)?
        .account_id
        .as_deref()?;
    snapshot
        .accounts
        .iter()
        .find(|a| a.id == id && a.harness == harness)
}

/// Label of the managed account `chat_id` is pinned to; `None` for Default or
/// no pin, so existing views stay quiet.
pub fn chat_account_badge(snapshot: &ManagedAccountsSnapshot, chat_id: &str) -> Option<String> {
    snapshot
        .chat_pins
        .iter()
        .filter(|pin| pin.chat_id == chat_id)
        .find_map(|pin| {
            let id = pin.account_id.as_deref()?;
            snapshot
                .accounts
                .iter()
                .find(|a| a.id == id && a.harness == pin.harness)
                .map(|a| a.label.clone())
        })
}

/// Managed accounts of one harness, in engine order.
pub fn accounts_for(
    snapshot: &ManagedAccountsSnapshot,
    harness: HarnessId,
) -> Vec<&ManagedAccount> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.harness == harness)
        .collect()
}

/// "Claude account: Work…" / "Codex account: Default…" (Space menu entry).
pub fn menu_label(
    snapshot: Option<&ManagedAccountsSnapshot>,
    space_id: &str,
    harness: HarnessId,
) -> String {
    let current = snapshot
        .and_then(|s| project_account(s, space_id, harness))
        .map(|a| a.label.as_str())
        .unwrap_or("Default");
    format!("{} account: {current}\u{2026}", harness_name(harness))
}

/// One selectable row of the project account picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickEntry {
    /// `None` = Default.
    pub account_id: Option<String>,
    pub label: String,
    pub detail: Option<String>,
    pub selected: bool,
    /// A saved Settings login (its slot id): choosing it promotes it into a
    /// project account. `account_id` is `None` for these.
    pub saved_slot: Option<String>,
    /// Shown but not choosable (the live Default login).
    pub disabled: bool,
}

/// Default plus only this harness's managed accounts.
pub fn pick_entries(
    snapshot: Option<&ManagedAccountsSnapshot>,
    space_id: &str,
    harness: HarnessId,
) -> Vec<PickEntry> {
    let empty = ManagedAccountsSnapshot::default();
    let snapshot = snapshot.unwrap_or(&empty);
    let current = project_account(snapshot, space_id, harness).map(|a| a.id.as_str());
    let mut entries = vec![PickEntry {
        account_id: None,
        label: "Default".into(),
        detail: None,
        selected: current.is_none(),
        saved_slot: None,
        disabled: false,
    }];
    entries.extend(
        accounts_for(snapshot, harness)
            .into_iter()
            .map(|a| PickEntry {
                account_id: Some(a.id.clone()),
                label: a.label.clone(),
                detail: Some(account_detail(a)),
                selected: current == Some(a.id.as_str()),
                saved_slot: None,
                disabled: false,
            }),
    );
    entries.extend(
        snapshot
            .saved
            .iter()
            .filter(|s| s.harness == harness)
            .map(|s| PickEntry {
                account_id: None,
                label: s.email.clone().unwrap_or_else(|| "Saved login".into()),
                detail: Some(if s.active {
                    "Your Default login".into()
                } else {
                    s.plan_label.clone().unwrap_or_else(|| "Saved login".into())
                }),
                selected: false,
                saved_slot: Some(s.id.clone()),
                disabled: s.active,
            }),
    );
    entries
}

/// Start Chat's account chip: the account the new chat will use, with the
/// choices that override it for this one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAccountChip {
    pub label: String,
    /// The pick differs from the project's default.
    pub overridden: bool,
    pub entries: Vec<PickEntry>,
}

/// The chip for a new chat of `harness` in `space_id`. `choice` is the
/// dialog's override (`None` = none; `Some(None)` = Default;
/// `Some(Some(id))` = that account). `None` when the harness has no
/// accounts or the device has no managed account for it, so the dialog
/// stays quiet for people who never added one.
pub fn start_chat_chip(
    snapshot: Option<&ManagedAccountsSnapshot>,
    space_id: &str,
    harness: HarnessId,
    choice: Option<&Option<String>>,
) -> Option<StartAccountChip> {
    if !supports_accounts(harness) {
        return None;
    }
    let snapshot = snapshot?;
    let accounts = accounts_for(snapshot, harness);
    if accounts.is_empty() {
        return None;
    }
    let default_id = project_account(snapshot, space_id, harness).map(|a| a.id.clone());
    let effective = match choice {
        Some(pick) => pick
            .clone()
            .filter(|id| accounts.iter().any(|a| &a.id == id)),
        None => default_id.clone(),
    };
    let label = effective
        .as_ref()
        .and_then(|id| accounts.iter().find(|a| &a.id == id))
        .map_or("Default", |a| a.label.as_str())
        .to_string();
    let mut entries = vec![PickEntry {
        account_id: None,
        label: "Default".into(),
        detail: None,
        selected: effective.is_none(),
        saved_slot: None,
        disabled: false,
    }];
    entries.extend(accounts.into_iter().map(|a| PickEntry {
        account_id: Some(a.id.clone()),
        label: a.label.clone(),
        detail: Some(account_detail(a)),
        selected: effective.as_deref() == Some(a.id.as_str()),
        saved_slot: None,
        disabled: false,
    }));
    Some(StartAccountChip {
        label,
        overridden: effective != default_id,
        entries,
    })
}

/// What the dialog stores after the user picks `account_id` (`None` =
/// Default): picking the project's own default clears the override.
pub fn start_chat_choice(
    snapshot: Option<&ManagedAccountsSnapshot>,
    space_id: &str,
    harness: HarnessId,
    account_id: Option<String>,
) -> Option<Option<String>> {
    let default_id = snapshot
        .and_then(|s| project_account(s, space_id, harness))
        .map(|a| a.id.clone());
    (account_id != default_id).then_some(account_id)
}

/// `CreateChatForWorkItem.accountId` for the dialog's override: absent when
/// there is none, empty for Default.
pub fn launch_account_id(harness: HarnessId, choice: Option<&Option<String>>) -> Option<String> {
    if !supports_accounts(harness) {
        return None;
    }
    choice.map(|pick| pick.clone().unwrap_or_default())
}

/// True when the snapshot offers any saved login for `harness`.
pub fn has_saved(snapshot: Option<&ManagedAccountsSnapshot>, harness: HarnessId) -> bool {
    snapshot.is_some_and(|s| s.saved.iter().any(|a| a.harness == harness))
}

pub fn promote_saved_params(
    space_id: &str,
    harness: HarnessId,
    slot_id: &str,
) -> serde_json::Value {
    serde_json::json!({ "spaceId": space_id, "harness": harness, "slotId": slot_id })
}

pub const SAVED_NOTE: &str = "Choosing a saved login moves it into this project's accounts, so it stops being offered for switching in Settings \u{2014} one sign-in can't safely live in two places. Your Default login can't be moved: use Add account to sign in separately.";

/// Email, or "Not signed in".
pub fn account_detail(account: &ManagedAccount) -> String {
    let base = match (&account.email, account.signed_in) {
        (Some(email), _) => email.clone(),
        _ => return "Not signed in".into(),
    };
    match account.plan_label.as_deref().filter(|p| !p.is_empty()) {
        Some(plan) => format!("{base} \u{b7} {plan}"),
        None => base,
    }
}

/// `ListManagedAccounts` params: the Settings page forces the usage probe on
/// a visit's first list, like the global accounts list does.
pub fn list_params(force_usage: bool) -> serde_json::Value {
    serde_json::json!({ "forceUsage": force_usage })
}

pub fn set_project_account_params(
    space_id: &str,
    harness: HarnessId,
    account_id: Option<&str>,
) -> serde_json::Value {
    let mut value = serde_json::json!({ "spaceId": space_id, "harness": harness });
    if let (Some(id), Some(object)) = (account_id, value.as_object_mut()) {
        object.insert("accountId".into(), id.into());
    }
    value
}

pub const PICKER_NOTE: &str =
    "Applies to new chats in this project. Existing chats keep the account they started with.";

// ---------------------------------------------------------------------------
// RPC plumbing + store (on AppState)
// ---------------------------------------------------------------------------

/// Tests: answer every managed-accounts call with this closure.
pub type ScriptedAccountsRpc =
    Arc<dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;

type RpcFuture = Pin<Box<dyn Future<Output = Result<serde_json::Value, String>>>>;

/// A cloneable handle for the managed-accounts RPCs, already targeted at the
/// device that owns the account home.
#[derive(Clone)]
pub struct AccountsRpc {
    engine: Option<crate::state::EngineHandle>,
    target: Option<String>,
    scripted: Option<ScriptedAccountsRpc>,
}

impl AccountsRpc {
    pub fn call(&self, method: &'static str, params: serde_json::Value) -> RpcFuture {
        let mut params = params;
        if let (Some(target), Some(object)) = (&self.target, params.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        if let Some(scripted) = self.scripted.clone() {
            return Box::pin(async move { scripted(method, params) });
        }
        let engine = self.engine.clone();
        Box::pin(async move {
            let Some(engine) = engine else {
                return Err("Engine not connected".to_string());
            };
            engine
                .client()
                .call(method, params)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

impl AppState {
    /// RPC handle for `device_id` (`None` = this device).
    pub(crate) fn accounts_rpc(&self, device_id: Option<&str>) -> AccountsRpc {
        let target = device_id
            .filter(|id| !self.is_local_device(id))
            .map(str::to_string);
        #[cfg(test)]
        let scripted = self.accounts_rpc_override.clone();
        #[cfg(not(test))]
        let scripted = None;
        AccountsRpc {
            engine: self.engine().cloned(),
            target,
            scripted,
        }
    }

    fn accounts_key(&self, device_id: Option<&str>) -> Option<String> {
        device_id
            .map(str::to_string)
            .or_else(|| self.local_device_id.clone())
    }

    pub fn managed_accounts_for(
        &self,
        device_id: Option<&str>,
    ) -> Option<&ManagedAccountsSnapshot> {
        let key = self.accounts_key(device_id)?;
        self.managed_accounts.get(&key)
    }

    /// Store a snapshot; true when it changed.
    pub fn set_managed_accounts(
        &mut self,
        device_id: Option<&str>,
        snapshot: ManagedAccountsSnapshot,
    ) -> bool {
        let Some(key) = self.accounts_key(device_id) else {
            return false;
        };
        if self.managed_accounts.get(&key) == Some(&snapshot) {
            return false;
        }
        self.managed_accounts.insert(key, snapshot);
        true
    }

    /// Badge text for a chat (its device's snapshot); `None` keeps views quiet.
    pub fn chat_account_badge(&self, chat_id: &str) -> Option<String> {
        let chat = self.chats.iter().find(|c| c.id == chat_id)?;
        let snapshot = self.managed_accounts.get(&chat.device_id)?;
        chat_account_badge(snapshot, chat_id)
    }

    /// Refresh the store from `device_id`'s engine (`None` = this device).
    pub fn load_managed_accounts(&mut self, device_id: Option<String>, cx: &mut Context<Self>) {
        if self.engine().is_none() && !cfg!(test) {
            return;
        }
        let rpc = self.accounts_rpc(device_id.as_deref());
        cx.spawn(async move |this, cx| {
            let Ok(value) = rpc
                .call(methods::LIST_MANAGED_ACCOUNTS, serde_json::json!({}))
                .await
            else {
                return;
            };
            let Ok(snapshot) = serde_json::from_value::<ManagedAccountsSnapshot>(value) else {
                return;
            };
            this.update(cx, |state, cx| {
                if state.set_managed_accounts(device_id.as_deref(), snapshot) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

// ---------------------------------------------------------------------------
// Shared flow entity
// ---------------------------------------------------------------------------

pub enum AccountFlowEvent {
    Closed,
}

enum Phase {
    /// Project picker: Default + this harness's accounts + Add account.
    Pick,
    /// Ask for the new account's name.
    Name,
    /// CreateManagedAccount / StartManagedAccountLogin in flight.
    Creating,
    /// Browser sign-in in progress; polling.
    Waiting {
        login_id: String,
    },
    /// PromoteSavedAccount in flight.
    Promoting,
    Failed(SharedString),
}

/// The sign-in sheet's view of the running CLI. Never holds the pasted code.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LoginView {
    pub url: Option<String>,
    pub opened: bool,
    pub awaiting_code: bool,
    pub device_code: Option<String>,
    pub submitting: bool,
    pub code_error: Option<String>,
}

pub struct AccountFlow {
    state: Entity<AppState>,
    harness: HarnessId,
    /// `Some` = project picker mode (sets the project's account); `None` =
    /// settings mode (add an account only).
    space_id: Option<String>,
    device_id: Option<String>,
    phase: Phase,
    name_input: Entity<ComposerInput>,
    code_input: Entity<ComposerInput>,
    /// What the running sign-in has printed so far (URL, code prompt).
    login: LoginView,
    focus_pending: bool,
    busy: bool,
    task: Option<Task<()>>,
    _events: Subscription,
    _code_events: Subscription,
}

impl EventEmitter<AccountFlowEvent> for AccountFlow {}

impl AccountFlow {
    pub fn new(
        state: Entity<AppState>,
        harness: HarnessId,
        space_id: Option<String>,
        device_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| ComposerInput::new("Account name", cx));
        let events = cx.subscribe(&name_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_name(cx);
            }
        });
        let code_input = cx.new(|cx| ComposerInput::new("Paste the code from your browser", cx));
        let code_events = cx.subscribe(&code_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_code(cx);
            }
        });
        let picking = space_id.is_some();
        let load_device = device_id.clone();
        state.update(cx, |state, cx| state.load_managed_accounts(load_device, cx));
        Self {
            state,
            harness,
            space_id,
            device_id,
            phase: if picking { Phase::Pick } else { Phase::Name },
            name_input,
            code_input,
            login: LoginView::default(),
            focus_pending: !picking,
            busy: false,
            task: None,
            _events: events,
            _code_events: code_events,
        }
    }

    fn rpc(&self, cx: &gpui::App) -> AccountsRpc {
        self.state.read(cx).accounts_rpc(self.device_id.as_deref())
    }

    fn store(&self, snapshot: ManagedAccountsSnapshot, cx: &mut Context<Self>) {
        let device = self.device_id.clone();
        self.state.update(cx, |state, cx| {
            if state.set_managed_accounts(device.as_deref(), snapshot) {
                cx.notify();
            }
        });
    }

    /// Choose Default (`None`) or a managed account for the project.
    pub fn select(&mut self, account_id: Option<String>, cx: &mut Context<Self>) {
        let Some(space_id) = self.space_id.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        let rpc = self.rpc(cx);
        let params = set_project_account_params(&space_id, self.harness, account_id.as_deref());
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = rpc.call(methods::SET_PROJECT_ACCOUNT, params).await;
            this.update(cx, |flow, cx| {
                flow.busy = false;
                match result.and_then(|v| {
                    serde_json::from_value::<ManagedAccountsSnapshot>(v).map_err(|e| e.to_string())
                }) {
                    Ok(snapshot) => {
                        flow.store(snapshot, cx);
                        cx.emit(AccountFlowEvent::Closed);
                    }
                    Err(err) => flow.phase = Phase::Failed(err.into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn begin_add(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Name;
        self.focus_pending = true;
        cx.notify();
    }

    /// Create the account, start its sign-in, open the browser and poll.
    pub fn submit_name(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Name) {
            return;
        }
        let label = self.name_input.read(cx).text().trim().to_string();
        if label.is_empty() {
            return;
        }
        self.phase = Phase::Creating;
        let rpc = self.rpc(cx);
        let harness = self.harness;
        let space_id = self.space_id.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let created = rpc
                .call(
                    methods::CREATE_MANAGED_ACCOUNT,
                    serde_json::json!({ "harness": harness, "label": label }),
                )
                .await
                .and_then(|v| {
                    serde_json::from_value::<ManagedAccount>(v).map_err(|e| e.to_string())
                });
            let account = match created {
                Ok(account) => account,
                Err(err) => {
                    this.update(cx, |flow, cx| {
                        flow.phase = Phase::Failed(err.into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let started = rpc
                .call(
                    methods::START_MANAGED_ACCOUNT_LOGIN,
                    serde_json::json!({ "accountId": account.id }),
                )
                .await
                .and_then(|v| {
                    serde_json::from_value::<AgentLoginStart>(v).map_err(|e| e.to_string())
                });
            let start = match started {
                Ok(start) => start,
                Err(err) => {
                    this.update(cx, |flow, cx| {
                        flow.phase =
                            Phase::Failed(format!("Sign-in failed to start: {err}").into());
                        flow.refresh(cx);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let login_id = start.login_id.clone();
            let opened = this.update(cx, |flow, cx| {
                flow.login = LoginView::default();
                if !start.url.is_empty() {
                    cx.open_url(&start.url);
                    flow.login.url = Some(start.url.clone());
                    flow.login.opened = true;
                }
                flow.phase = Phase::Waiting {
                    login_id: start.login_id.clone(),
                };
                flow.code_input
                    .update(cx, |input, cx| input.set_text("", cx));
                flow.focus_pending = true;
                cx.notify();
            });
            if opened.is_err() {
                return;
            }
            // Poll every 1.5s until Done / Error.
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                let polled = rpc
                    .call(
                        methods::POLL_MANAGED_ACCOUNT_LOGIN,
                        serde_json::json!({ "loginId": login_id }),
                    )
                    .await
                    .and_then(|v| {
                        serde_json::from_value::<AgentLoginPoll>(v).map_err(|e| e.to_string())
                    });
                match polled {
                    Ok(poll) => match poll.status {
                        AgentLoginStatus::Pending => {
                            let alive = this.update(cx, |flow, cx| {
                                flow.apply_poll(&poll, cx);
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        AgentLoginStatus::Error => {
                            let message = poll.message.unwrap_or_else(|| "Sign-in failed".into());
                            this.update(cx, |flow, cx| {
                                flow.phase = Phase::Failed(message.into());
                                flow.refresh(cx);
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                        AgentLoginStatus::Done => {
                            let set = match &space_id {
                                Some(space_id) => Some(
                                    rpc.call(
                                        methods::SET_PROJECT_ACCOUNT,
                                        set_project_account_params(
                                            space_id,
                                            harness,
                                            Some(&account.id),
                                        ),
                                    )
                                    .await,
                                ),
                                None => None,
                            };
                            let snapshot = match set {
                                Some(Ok(v)) => serde_json::from_value(v).ok(),
                                Some(Err(err)) => {
                                    this.update(cx, |flow, cx| {
                                        flow.phase = Phase::Failed(err.into());
                                        flow.refresh(cx);
                                        cx.notify();
                                    })
                                    .ok();
                                    return;
                                }
                                None => None,
                            };
                            this.update(cx, |flow, cx| {
                                match snapshot {
                                    Some(snapshot) => flow.store(snapshot, cx),
                                    None => flow.refresh(cx),
                                }
                                cx.emit(AccountFlowEvent::Closed);
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    },
                    Err(err) => {
                        this.update(cx, |flow, cx| {
                            flow.phase = Phase::Failed(format!("Poll failed: {err}").into());
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                }
            }
        }));
        cx.notify();
    }

    /// Fold a pending poll into the sheet; the URL opens once, when it first appears.
    pub(crate) fn apply_poll(&mut self, poll: &AgentLoginPoll, cx: &mut Context<Self>) {
        let mut next = self.login.clone();
        if let Some(url) = &poll.url {
            if !next.opened {
                cx.open_url(url);
                next.opened = true;
            }
            next.url = Some(url.clone());
        }
        next.awaiting_code = poll.awaiting_code;
        next.device_code = poll.device_code.clone();
        if next != self.login {
            self.login = next;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting_for_test(&mut self, login_id: &str) {
        self.phase = Phase::Waiting {
            login_id: login_id.into(),
        };
    }

    #[cfg(test)]
    pub(crate) fn set_code_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.code_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    #[cfg(test)]
    pub(crate) fn code_text_for_test(&self, cx: &gpui::App) -> String {
        self.code_input.read(cx).text().to_string()
    }

    #[cfg(test)]
    pub(crate) fn login_view(&self) -> &LoginView {
        &self.login
    }

    /// Write the pasted code to the sign-in process (never stored or logged).
    pub fn submit_code(&mut self, cx: &mut Context<Self>) {
        let Phase::Waiting { login_id } = &self.phase else {
            return;
        };
        let code = self.code_input.read(cx).text().trim().to_string();
        if code.is_empty() || self.login.submitting {
            return;
        }
        let login_id = login_id.clone();
        self.login.submitting = true;
        self.login.code_error = None;
        self.code_input
            .update(cx, |input, cx| input.set_text("", cx));
        let rpc = self.rpc(cx);
        cx.spawn(async move |this, cx| {
            let result = rpc
                .call(
                    methods::SUBMIT_MANAGED_ACCOUNT_LOGIN_CODE,
                    serde_json::json!({ "loginId": login_id, "code": code }),
                )
                .await;
            this.update(cx, |flow, cx| {
                flow.login.submitting = false;
                if let Err(err) = result {
                    flow.login.code_error = Some(err);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn copy_url(&self, cx: &mut Context<Self>) {
        if let Some(url) = &self.login.url {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
        }
    }

    /// Promote a saved Settings login into a project account and use it.
    pub fn promote(&mut self, slot_id: String, cx: &mut Context<Self>) {
        let Some(space_id) = self.space_id.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        self.phase = Phase::Promoting;
        let rpc = self.rpc(cx);
        let params = promote_saved_params(&space_id, self.harness, &slot_id);
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = rpc.call(methods::PROMOTE_SAVED_ACCOUNT, params).await;
            this.update(cx, |flow, cx| {
                flow.busy = false;
                match result.and_then(|v| {
                    serde_json::from_value::<ManagedAccountsSnapshot>(v).map_err(|e| e.to_string())
                }) {
                    Ok(snapshot) => {
                        flow.store(snapshot, cx);
                        cx.emit(AccountFlowEvent::Closed);
                    }
                    Err(err) => {
                        flow.phase = Phase::Failed(err.into());
                        flow.refresh(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn refresh(&self, cx: &mut Context<Self>) {
        let device = self.device_id.clone();
        self.state
            .update(cx, |state, cx| state.load_managed_accounts(device, cx));
    }

    /// Cancel / close: best-effort CancelManagedAccountLogin while waiting.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Phase::Waiting { login_id } = &self.phase {
            let rpc = self.rpc(cx);
            let params = serde_json::json!({ "loginId": login_id });
            cx.spawn(async move |_, _| {
                let _ = rpc
                    .call(methods::CANCEL_MANAGED_ACCOUNT_LOGIN, params)
                    .await;
            })
            .detach();
        }
        self.task = None;
        self.refresh(cx);
        cx.emit(AccountFlowEvent::Closed);
    }

    /// The dialog card (wrap in `popover::modal`).
    pub fn render_card(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = harness_name(self.harness);
        let (title, body): (String, AnyElement) = match &self.phase {
            Phase::Pick => (
                format!("{name} account for this project"),
                self.render_pick(theme, cx),
            ),
            Phase::Name => {
                if std::mem::take(&mut self.focus_pending) {
                    window.focus(&self.name_input.focus_handle(cx), cx);
                }
                (
                    format!("Add {name} account"),
                    div()
                        .flex()
                        .flex_col()
                        .child(div().mt(px(12.0)).child(popover::dialog_field(
                            self.name_input.clone().into_any_element(),
                        )))
                        .child(
                            div()
                                .mt(px(16.0))
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(8.0))
                                .child(
                                    popover::btn_ghost(theme, "Cancel", "account-flow-cancel")
                                        .id("account-flow-cancel")
                                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                                )
                                .child(
                                    popover::btn_primary(theme, "Continue")
                                        .id("account-flow-continue")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.submit_name(cx)),
                                        ),
                                ),
                        )
                        .into_any_element(),
                )
            }
            Phase::Creating => (
                format!("Add {name} account"),
                self.status_body(theme, "Setting up the account\u{2026}", "Cancel", cx),
            ),
            Phase::Waiting { .. } => {
                if self.login.awaiting_code && std::mem::take(&mut self.focus_pending) {
                    window.focus(&self.code_input.focus_handle(cx), cx);
                }
                (
                    format!("Add {name} account"),
                    self.render_waiting(theme, cx),
                )
            }
            Phase::Promoting => (
                format!("Add {name} account"),
                self.status_body(theme, "Moving the saved login\u{2026}", "Cancel", cx),
            ),
            Phase::Failed(message) => (
                format!("Add {name} account"),
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .mt(px(10.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.danger_muted.opacity(0.9))
                            .child(message.clone()),
                    )
                    .child(self.close_row(theme, "Close", cx))
                    .into_any_element(),
            ),
        };
        popover::dialog_card(theme)
            .child(popover::dialog_title(theme, &title))
            .child(body)
            .into_any_element()
    }

    /// The sign-in sheet: status, the authorize URL (open / copy), a device
    /// code if the CLI printed one, and the paste-code field once it asks.
    fn render_waiting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let login = self.login.clone();
        let status = if login.submitting {
            "Checking the code\u{2026}"
        } else if login.url.is_none() {
            "Starting sign-in\u{2026}"
        } else if login.awaiting_code {
            "Finish in your browser, then paste the code it shows."
        } else {
            "Waiting for sign-in in your browser\u{2026}"
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .mt(px(14.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(crate::loaders::gradient_spinner(
                        "account-flow-wait",
                        theme,
                        3.0,
                        cx.entity_id(),
                        cx,
                    ))
                    .child(
                        div()
                            .id("account-flow-status")
                            .text_size(crate::typography::ui_rems(12.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(status)),
                    ),
            )
            .when_some(login.url.clone(), |el, url| {
                el.child(
                    div()
                        .mt(px(12.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .id("account-flow-url")
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(url.clone())),
                        )
                        .child(
                            popover::btn_ghost(theme, "Open", "account-flow-open")
                                .id("account-flow-open")
                                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                        )
                        .child(
                            popover::btn_ghost(theme, "Copy", "account-flow-copy")
                                .id("account-flow-copy")
                                .on_click(cx.listener(|this, _, _, cx| this.copy_url(cx))),
                        ),
                )
            })
            .when_some(login.device_code.clone(), |el, code| {
                el.child(
                    div()
                        .id("account-flow-device-code")
                        .mt(px(10.0))
                        .text_size(crate::typography::ui_rems(12.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!(
                            "Enter this code in the browser: {code}"
                        ))),
                )
            })
            .when(login.awaiting_code, |el| {
                el.child(div().mt(px(12.0)).child(popover::dialog_field(
                    self.code_input.clone().into_any_element(),
                )))
                .child(
                    div().mt(px(10.0)).flex().flex_row().justify_end().child(
                        popover::btn_primary(theme, "Submit")
                            .id("account-flow-submit-code")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_code(cx))),
                    ),
                )
            })
            .when_some(login.code_error.clone(), |el, err| {
                el.child(
                    div()
                        .id("account-flow-code-error")
                        .mt(px(8.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.danger_muted.opacity(0.9))
                        .child(SharedString::from(err)),
                )
            })
            .child(self.close_row(theme, "Cancel", cx))
            .into_any_element()
    }

    fn close_row(&self, theme: &Theme, label: &str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .mt(px(16.0))
            .flex()
            .flex_row()
            .justify_end()
            .child(
                popover::btn_ghost(theme, label, "account-flow-cancel")
                    .id("account-flow-cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            )
            .into_any_element()
    }

    fn status_body(
        &self,
        theme: &Theme,
        text: &'static str,
        button: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(crate::loaders::gradient_spinner(
                        "account-flow-wait",
                        theme,
                        3.0,
                        cx.entity_id(),
                        cx,
                    ))
                    .child(
                        div()
                            .id("account-flow-status")
                            .text_size(crate::typography::ui_rems(12.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(text)),
                    ),
            )
            .child(self.close_row(theme, button, cx))
            .into_any_element()
    }

    fn render_pick(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let space_id = self.space_id.clone().unwrap_or_default();
        let saved_present = has_saved(
            self.state
                .read(cx)
                .managed_accounts_for(self.device_id.as_deref()),
            self.harness,
        );
        let entries = {
            let state = self.state.read(cx);
            pick_entries(
                state.managed_accounts_for(self.device_id.as_deref()),
                &space_id,
                self.harness,
            )
        };
        let rows = entries.into_iter().map(|entry| {
            let key = match &entry.account_id {
                None => "account-pick-default".to_string(),
                Some(id) => format!("account-pick-{id}"),
            };
            let select = entry.account_id.clone();
            let saved = entry.saved_slot.clone();
            let disabled = entry.disabled;
            let key = match &saved {
                Some(slot) => format!("account-pick-saved-{slot}"),
                None => key,
            };
            let debug_key = key.clone();
            popover::menu_row(theme, entry.selected, key.clone())
                .id(SharedString::from(key))
                .debug_selector(move || debug_key.clone())
                .when(disabled, |el| el.opacity(0.5))
                .when(!disabled, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| match &saved {
                        Some(slot) => this.promote(slot.clone(), cx),
                        None => this.select(select.clone(), cx),
                    }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(entry.label)),
                )
                .when_some(entry.detail, |el, detail| {
                    el.child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(detail)),
                    )
                })
                .when(entry.selected, |el| {
                    el.child(
                        crate::icons::icon(crate::icons::CHECK)
                            .size(px(14.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                })
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .mt(px(6.0))
                    .child(popover::dialog_body(theme, PICKER_NOTE)),
            )
            .child(div().mt(px(10.0)).flex().flex_col().children(rows))
            .when(saved_present, |el| {
                el.child(
                    div()
                        .id("account-pick-saved-note")
                        .mt(px(8.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(SAVED_NOTE)),
                )
            })
            .child(popover::menu_separator())
            .child(
                popover::menu_row(theme, false, "account-pick-add")
                    .id("account-pick-add")
                    .debug_selector(|| "account-pick-add".into())
                    .on_click(cx.listener(|this, _, _, cx| this.begin_add(cx)))
                    .child(
                        crate::icons::icon(crate::icons::ADD_CIRCLE)
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from("Add account\u{2026}")),
            )
            .child(self.close_row(theme, "Close", cx))
            .into_any_element()
    }
}

/// Quiet neutral pill (the sidebar's small badge cloth) showing an account.
pub fn account_pill(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    let tone = theme.text_muted;
    div()
        .h(px(16.0))
        .flex_none()
        .max_w(px(90.0))
        .truncate()
        .flex()
        .flex_row()
        .items_center()
        .px(px(4.0))
        .rounded(px(4.0))
        .bg(tone.opacity(0.08))
        .text_size(crate::typography::ui_rems(10.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(tone.opacity(0.85))
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::SavedAccountOption;
    use mast_proto::{ChatAccountPin, ProjectAccountDefault};

    fn account(id: &str, harness: HarnessId, label: &str) -> ManagedAccount {
        ManagedAccount {
            id: id.into(),
            harness,
            label: label.into(),
            email: None,
            signed_in: false,
            plan_label: None,
            usage_windows: Vec::new(),
        }
    }

    fn snapshot() -> ManagedAccountsSnapshot {
        ManagedAccountsSnapshot {
            accounts: vec![
                account("a1", HarnessId::ClaudeCode, "Work"),
                account("a2", HarnessId::Codex, "Side"),
            ],
            project_defaults: vec![ProjectAccountDefault {
                space_id: "s1".into(),
                harness: HarnessId::ClaudeCode,
                account_id: Some("a1".into()),
            }],
            chat_pins: vec![
                ChatAccountPin {
                    chat_id: "c1".into(),
                    harness: HarnessId::ClaudeCode,
                    account_id: Some("a1".into()),
                },
                ChatAccountPin {
                    chat_id: "c2".into(),
                    harness: HarnessId::ClaudeCode,
                    account_id: None,
                },
            ],
            saved: vec![
                SavedAccountOption {
                    id: "slot1".into(),
                    harness: HarnessId::ClaudeCode,
                    email: Some("saved@example.com".into()),
                    plan_label: Some("Max".into()),
                    active: false,
                },
                SavedAccountOption {
                    id: "slot2".into(),
                    harness: HarnessId::ClaudeCode,
                    email: Some("live@example.com".into()),
                    plan_label: None,
                    active: true,
                },
            ],
        }
    }

    #[test]
    fn saved_logins_follow_managed_accounts_and_the_live_one_is_disabled() {
        let snap = snapshot();
        let entries = pick_entries(Some(&snap), "s1", HarnessId::ClaudeCode);
        let saved: Vec<_> = entries.iter().filter(|e| e.saved_slot.is_some()).collect();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].label, "saved@example.com");
        assert_eq!(saved[0].detail.as_deref(), Some("Max"));
        assert!(!saved[0].disabled && saved[0].account_id.is_none());
        assert!(saved[1].disabled);
        assert_eq!(saved[1].detail.as_deref(), Some("Your Default login"));
        // Saved rows come after the managed ones.
        assert_eq!(entries[1].account_id.as_deref(), Some("a1"));
        assert!(
            pick_entries(Some(&snap), "s1", HarnessId::Codex)
                .iter()
                .all(|e| e.saved_slot.is_none())
        );
        assert!(has_saved(Some(&snap), HarnessId::ClaudeCode));
        assert!(!has_saved(Some(&snap), HarnessId::Codex));
        assert_eq!(
            promote_saved_params("s1", HarnessId::ClaudeCode, "slot1"),
            serde_json::json!({ "spaceId": "s1", "harness": "claude-code", "slotId": "slot1" })
        );
    }

    #[test]
    fn project_account_resolves_per_harness_and_defaults_to_none() {
        let snap = snapshot();
        assert_eq!(
            project_account(&snap, "s1", HarnessId::ClaudeCode).map(|a| a.label.as_str()),
            Some("Work")
        );
        assert!(project_account(&snap, "s1", HarnessId::Codex).is_none());
        assert!(project_account(&snap, "s2", HarnessId::ClaudeCode).is_none());
    }

    #[test]
    fn chat_badge_only_for_pinned_managed_accounts() {
        let snap = snapshot();
        assert_eq!(chat_account_badge(&snap, "c1").as_deref(), Some("Work"));
        assert_eq!(chat_account_badge(&snap, "c2"), None);
        assert_eq!(chat_account_badge(&snap, "unpinned"), None);
    }

    #[test]
    fn pick_entries_list_default_and_only_that_harness() {
        let snap = snapshot();
        let claude = pick_entries(Some(&snap), "s1", HarnessId::ClaudeCode);
        let labels: Vec<_> = claude
            .iter()
            .filter(|e| e.saved_slot.is_none())
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, ["Default", "Work"]);
        assert!(!claude[0].selected && claude[1].selected);
        let codex = pick_entries(Some(&snap), "s1", HarnessId::Codex);
        let labels: Vec<_> = codex.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Default", "Side"]);
        assert!(codex[0].selected);
        assert_eq!(codex[1].detail.as_deref(), Some("Not signed in"));
    }

    #[test]
    fn menu_labels_and_params() {
        let snap = snapshot();
        assert_eq!(
            menu_label(Some(&snap), "s1", HarnessId::ClaudeCode),
            "Claude account: Work\u{2026}"
        );
        assert_eq!(
            menu_label(None, "s1", HarnessId::Codex),
            "Codex account: Default\u{2026}"
        );
        assert_eq!(
            set_project_account_params("s1", HarnessId::Codex, None),
            serde_json::json!({ "spaceId": "s1", "harness": "codex" })
        );
        assert_eq!(
            set_project_account_params("s1", HarnessId::ClaudeCode, Some("a1"))["accountId"],
            "a1"
        );
    }

    #[test]
    fn account_detail_shows_the_probed_plan_after_the_email() {
        let mut a = account("a1", HarnessId::ClaudeCode, "Work");
        assert_eq!(account_detail(&a), "Not signed in");
        a.signed_in = true;
        a.email = Some("me@example.com".into());
        assert_eq!(account_detail(&a), "me@example.com");
        a.plan_label = Some("Max".into());
        assert_eq!(account_detail(&a), "me@example.com \u{b7} Max");
    }

    #[test]
    fn list_params_carry_the_force_usage_flag() {
        assert_eq!(list_params(true), serde_json::json!({ "forceUsage": true }));
        assert_eq!(
            list_params(false),
            serde_json::json!({ "forceUsage": false })
        );
    }

    #[test]
    fn start_chat_chip_shows_the_project_default_and_marks_overrides() {
        let snap = snapshot();
        let chip = start_chat_chip(Some(&snap), "s1", HarnessId::ClaudeCode, None).unwrap();
        assert_eq!(chip.label, "Work");
        assert!(!chip.overridden);
        let selected: Vec<_> = chip.entries.iter().filter(|e| e.selected).collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].account_id.as_deref(), Some("a1"));
        // Default login picked for this one chat.
        let chip = start_chat_chip(Some(&snap), "s1", HarnessId::ClaudeCode, Some(&None)).unwrap();
        assert_eq!(chip.label, "Default");
        assert!(chip.overridden);
        assert!(chip.entries[0].selected);
        // A project with no default shows Default, not overridden.
        let chip = start_chat_chip(Some(&snap), "s2", HarnessId::ClaudeCode, None).unwrap();
        assert_eq!(chip.label, "Default");
        assert!(!chip.overridden);
    }

    #[test]
    fn start_chat_chip_is_quiet_without_accounts_to_choose_from() {
        let snap = snapshot();
        assert!(start_chat_chip(None, "s1", HarnessId::ClaudeCode, None).is_none());
        assert!(start_chat_chip(Some(&snap), "s1", HarnessId::Cursor, None).is_none());
        let empty = ManagedAccountsSnapshot::default();
        assert!(start_chat_chip(Some(&empty), "s1", HarnessId::Codex, None).is_none());
    }

    #[test]
    fn picking_the_projects_own_default_clears_the_override() {
        let snap = snapshot();
        let pick = |id: Option<&str>| {
            start_chat_choice(
                Some(&snap),
                "s1",
                HarnessId::ClaudeCode,
                id.map(str::to_string),
            )
        };
        assert_eq!(pick(Some("a1")), None);
        assert_eq!(pick(None), Some(None));
        // Another project (no default) treats Default as the baseline.
        assert_eq!(
            start_chat_choice(Some(&snap), "s2", HarnessId::ClaudeCode, None),
            None
        );
    }

    #[test]
    fn launch_account_id_is_absent_without_an_override_and_empty_for_default() {
        assert_eq!(launch_account_id(HarnessId::ClaudeCode, None), None);
        assert_eq!(
            launch_account_id(HarnessId::ClaudeCode, Some(&None)).as_deref(),
            Some("")
        );
        assert_eq!(
            launch_account_id(HarnessId::Codex, Some(&Some("a2".into()))).as_deref(),
            Some("a2")
        );
        assert_eq!(launch_account_id(HarnessId::Cursor, Some(&None)), None);
    }
}
