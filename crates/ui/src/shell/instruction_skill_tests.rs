// `/` skill lists of the Work Item Details instruction editor, driven through
// the real Shell (keymap, composer pickers, Overview, Details), for the live
// "Design" item's shape: a root-level item whose execution root is the `ui`
// submodule below the Space path, bound as a member that is NOT primary.
// Included from `work_outline_ui.rs`'s test module so it shares its fixtures.
use super::*;

use crate::composer::ComposerInput;
use crate::composer::skill_completion::SkillCompletion;
use crate::work_outline::store::fixture_ids::*;

type Calls = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

const SPACE: &str = "/work/alpha";
const UI_ROOT: &str = "/work/alpha/ui";

/// Discovery answers by (harness, folder): Claude Code's `ui` list is what
/// the running engine answered on 2026-10-14 (trimmed to the names that
/// matter, built-ins included); Codex answers its personal skills only (it
/// never reads `.claude/skills`), like the live engine does. A folder named
/// `broken` fails.
fn discovery(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
    let cwd = params["cwd"].as_str().unwrap_or_default();
    if cwd.ends_with("/broken") {
        return Err(mast_rpc::RpcError::Failed(
            "claude initialize failed".into(),
        ));
    }
    let names: &[(&str, &str)] = match params["harness"].as_str() {
        Some("claude-code") if cwd.ends_with("/ui") => &[
            ("__remote-workflow", ""),
            ("accessibility", "Accessibility checks"),
            ("clear", "Clear the conversation"),
            ("code-review", "Review a diff"),
            ("compact", "Compact the conversation"),
            ("design", "Engine: design"),
            (
                "design-consent",
                "Grant Claude agent access to your Design projects",
            ),
            (
                "design-revoke",
                "Revoke Claude agent access to your Design projects",
            ),
            ("design-sync", "Push a React design system"),
            ("design-system", "Engine: design-system"),
            ("model", "Switch the model"),
            ("reload-skills", "Reload skills"),
        ],
        Some("claude-code") => &[
            ("code-review", "Review a diff"),
            ("db-query", "Query the db"),
        ],
        Some("codex") => &[("code-review", "Review a diff"), ("talk-to-me", "Style")],
        _ => &[],
    };
    Ok(serde_json::Value::Array(
        names
            .iter()
            .map(
                |(name, description)| serde_json::json!({"name": name, "description": description}),
            )
            .collect(),
    ))
}

/// The fixture snapshot with Housekeeping (a root-level item, no Chats,
/// like Design) rooted at `root`: one member binding, not primary.
fn rooted_snapshot(root: &str) -> mast_proto::WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot.execution_space = Some(mast_proto::ExecutionSpace {
        space_id: "s1".into(),
        path: SPACE.into(),
        device_id: "d".into(),
    });
    let scope = mast_proto::ExecutionScope::WorkItem {
        work_item_id: HOUSEKEEPING.into(),
    };
    snapshot
        .execution_configurations
        .push(mast_proto::ExecutionConfigurationView {
            id: scope.row_id(),
            space_id: "s1".into(),
            work_item_id: Some(HOUSEKEEPING.into()),
            configuration: Some(mast_proto::ExecutionConfiguration {
                execution_root: root.into(),
                bindings: vec![mast_proto::RepositoryBinding {
                    repo_id: "repo:0081d8ad".into(),
                    repo_fingerprint: "d0848554".into(),
                    repo_path: root.into(),
                    member: true,
                    primary: false,
                    worktree_id: Some("0081d8ad".into()),
                    worktree_path: Some(root.into()),
                    expected_ref: None,
                }],
                schema: 1,
            }),
            invalid_reason: None,
            updated_by: None,
            updated_at: 1,
        });
    snapshot
}

fn set_root(
    overview: &Entity<crate::overview::Overview>,
    root: &str,
    cx: &mut gpui::VisualTestContext,
) {
    let snapshot = rooted_snapshot(root);
    overview.update(cx, |overview, cx| {
        overview.set_outline_loads(
            std::collections::HashMap::from([(
                "s1".to_string(),
                crate::work_outline::store::WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
            )]),
            cx,
        )
    });
    redraw(cx);
}

/// Scripted discovery, a selected Codex Chat (the Shell's composer follows
/// it), and Housekeeping rooted at `root`. `local` marks device `d` as this
/// device, so the editor may read the root's files.
fn seed(
    shell: &Entity<Shell>,
    overview: &Entity<crate::overview::Overview>,
    root: &str,
    local: bool,
    cx: &mut gpui::VisualTestContext,
) -> Calls {
    let calls: Calls = Default::default();
    let sink = calls.clone();
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.work_outline_actions_override =
                Some(crate::work_outline::WorkOutlineActions::scripted(
                    std::sync::Arc::new(move |method, params| {
                        if method == mast_rpc::methods::LIST_COMMANDS {
                            sink.lock().unwrap().push(params.clone());
                            return discovery(&params);
                        }
                        Ok(serde_json::json!({}))
                    }),
                ));
            for chat in &mut state.chats {
                if chat.id == CHAT_AUTH {
                    chat.config = Some(mast_proto::ChatConfig {
                        harness: mast_proto::HarnessId::Codex,
                        model: None,
                        reasoning: None,
                        model_options: Default::default(),
                        sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
                        auto_approve: false,
                    });
                }
            }
            state.selected_chat = Some(CHAT_AUTH.into());
            if local {
                state.local_device_id = Some("d".into());
            }
        })
    });
    set_root(overview, root, cx);
    calls
}

/// Open Housekeeping's Details through the Overview (as the Outline's "Add
/// instruction" does) with a new instruction draft focused.
fn open_instruction_editor(
    overview: &Entity<crate::overview::Overview>,
    cx: &mut gpui::VisualTestContext,
) -> (Entity<ComposerInput>, Entity<SkillCompletion>) {
    let editor = std::rc::Rc::new(std::cell::RefCell::new(None));
    let sink = editor.clone();
    overview.update_in(cx, |overview, window, cx| {
        overview.with_work_item_details(
            "s1".into(),
            HOUSEKEEPING.into(),
            window,
            cx,
            move |details, window, cx| {
                details.begin_instruction_draft(None, window, cx);
                *sink.borrow_mut() = Some(details.editor("instruction"));
            },
        )
    });
    redraw(cx);
    let (input, skills) = editor.borrow_mut().take().expect("details opened");
    assert!(cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)));
    (input, skills.unwrap())
}

fn names(skills: &Entity<SkillCompletion>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
    skills.read_with(cx, |skills, _| {
        if skills.is_open() {
            skills.visible_names()
        } else {
            Vec::new()
        }
    })
}

/// The live report, end to end: with a Codex Chat selected, the Design
/// item's instruction editor still asks Claude Code discovery for the item's
/// execution root (`ui`, though its only binding is not primary) on the
/// Space's device, and typing `/design` (real keystrokes) lists `design`,
/// then the other `design*` skills; built-ins never show.
#[gpui::test]
fn design_item_instruction_editor_lists_the_ui_skills_whatever_chat_is_selected(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, overview, _outline, cx) = toggle_shell(cx, dir.path());
    let calls = seed(&shell, &overview, UI_ROOT, false, cx);
    assert_eq!(
        shell.read_with(cx, |shell, cx| shell.skill_harness(cx)),
        mast_proto::HarnessId::Codex,
        "the composer follows the selected Codex Chat"
    );
    let (input, skills) = open_instruction_editor(&overview, cx);
    cx.simulate_keystrokes("/");
    redraw(cx);
    let lists = calls.lock().unwrap().clone();
    assert_eq!(lists.len(), 1, "{lists:?}");
    assert_eq!(
        lists[0]["harness"], "claude-code",
        "never the selected Chat's harness"
    );
    assert_eq!(
        lists[0]["cwd"], UI_ROOT,
        "the execution root, not the Space path"
    );
    assert_eq!(lists[0]["targetDeviceId"], "d");
    assert_eq!(
        names(&skills, cx),
        [
            "accessibility",
            "code-review",
            "design",
            "design-sync",
            "design-system"
        ],
        "built-ins (clear, compact, model, reload-skills, design-consent, __…) dropped"
    );

    cx.simulate_keystrokes("d e s i g n");
    redraw(cx);
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_owned()),
        "/design"
    );
    assert_eq!(
        names(&skills, cx),
        ["design", "design-sync", "design-system"]
    );
    assert!(cx.debug_bounds("skill-completion-row-0").is_some());
    assert_eq!(calls.lock().unwrap().len(), 1, "filtering is local");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert_eq!(
        input.read_with(cx, |input, _| input.text().to_owned()),
        "`design` skill "
    );
}

fn write_skill(dir: &std::path::Path, folder: &str, frontmatter: &str) {
    let path = dir.join(".claude").join("skills").join(folder);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("SKILL.md"),
        format!("---\n{frontmatter}\n---\n# Body\n"),
    )
    .unwrap();
}

/// On this device the list is the union of discovery and the root's own
/// `SKILL.md` files (up to the submodule's git root) plus `~/.claude/skills`,
/// one row per name with the project's skill winning; a non-invocable skill
/// is hidden; and when discovery fails the files still list.
#[gpui::test]
fn instruction_editor_unions_discovery_with_the_roots_skill_files(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let workspace = files.path().join("workspace");
    let ui = workspace.join("ui");
    let home = files.path().join("home");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(ui.join(".git"), "gitdir: ../.git/modules/ui\n").unwrap();
    write_skill(&workspace, "db-query", "name: db-query");
    write_skill(
        &ui,
        "design",
        "name: design\ndescription: Project: design\nuser-invocable: true",
    );
    write_skill(
        &ui,
        "design-system",
        "name: design-system\ndescription: Project: allowlist",
    );
    write_skill(&ui, "hidden", "name: hidden\nuser-invocable: false");
    write_skill(&home, "design", "name: design\ndescription: User: design");
    write_skill(
        &home,
        "my-voice",
        "name: my-voice\ndescription: User: voice",
    );
    let ui_root = ui.to_string_lossy().into_owned();

    let (shell, overview, _outline, cx) = toggle_shell(cx, dir.path());
    let calls = seed(&shell, &overview, &ui_root, true, cx);
    let (_input, skills) = open_instruction_editor(&overview, cx);
    skills.update(cx, |skills, _| skills.set_home(Some(home.clone())));
    cx.simulate_keystrokes("/");
    redraw(cx);
    assert_eq!(calls.lock().unwrap()[0]["cwd"], ui_root.as_str());
    let rows = skills.read_with(cx, |skills, _| skills.visible_rows());
    let listed: Vec<(&str, &str)> = rows
        .iter()
        .map(|(name, description)| (name.as_str(), description.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("accessibility", "Accessibility checks"),
            ("code-review", "Review a diff"),
            ("design", "Project: design"),
            ("design-sync", "Push a React design system"),
            ("design-system", "Project: allowlist"),
            ("my-voice", "User: voice"),
        ],
        "deduped by name, project files first; db-query is above the git root"
    );
    cx.simulate_keystrokes("d e s");
    redraw(cx);
    assert_eq!(
        names(&skills, cx),
        ["design", "design-sync", "design-system"]
    );
    cx.simulate_keystrokes("escape");
    redraw(cx);

    // Discovery fails for this root: its files still list, no error.
    let broken = files.path().join("broken");
    write_skill(&broken, "local-only", "name: local-only");
    set_root(&overview, &broken.to_string_lossy(), cx);
    cx.simulate_keystrokes("space /");
    redraw(cx);
    let rows = skills.read_with(cx, |skills, _| skills.visible_rows());
    assert_eq!(
        rows.iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["design", "local-only", "my-voice"]
    );
    assert_eq!(rows[0].1, "User: design", "no project `design` here");
    assert!(skills.read_with(cx, |skills, _| skills.error_text().is_none()));
}

/// The list is cached per (device, root): reopening on the same root does
/// not fetch again, a root change fetches the new root, and returning to the
/// first root fetches it afresh (the old root's list was dropped).
#[gpui::test]
fn instruction_editor_cache_follows_the_execution_root(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, overview, _outline, cx) = toggle_shell(cx, dir.path());
    let calls = seed(&shell, &overview, UI_ROOT, false, cx);
    let (_input, skills) = open_instruction_editor(&overview, cx);
    let cwds = |calls: &Calls| -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|params| params["cwd"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    cx.simulate_keystrokes("/");
    redraw(cx);
    cx.simulate_keystrokes("escape space /");
    redraw(cx);
    assert_eq!(cwds(&calls), [UI_ROOT], "reopened from the cache");
    assert!(names(&skills, cx).contains(&"design".to_owned()));
    cx.simulate_keystrokes("escape");

    set_root(&overview, SPACE, cx);
    cx.simulate_keystrokes("space /");
    redraw(cx);
    assert_eq!(cwds(&calls), [UI_ROOT, SPACE]);
    assert_eq!(names(&skills, cx), ["code-review", "db-query"]);
    cx.simulate_keystrokes("escape");

    set_root(&overview, UI_ROOT, cx);
    cx.simulate_keystrokes("space /");
    redraw(cx);
    assert_eq!(cwds(&calls), [UI_ROOT, SPACE, UI_ROOT], "fetched afresh");
    assert!(names(&skills, cx).contains(&"design".to_owned()));
}
