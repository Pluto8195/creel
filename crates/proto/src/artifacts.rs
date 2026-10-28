//! Planning artifacts wire contract (version 1, phase 1 = design A + B).
//!
//! Contract doc: `docs/plans/artifacts-contract.md`. Agent conventions:
//! `docs/planning-artifact-conventions.md`. Design:
//! `docs/plans/planning-artifacts-design.md`.
//!
//! - An artifact is a file Zeron owns under the work item's canonical
//!   checkout: `<canonical>/.zeron/artifacts/<folder>/<file>` (chats without a
//!   work item: `<canonical>/.zeron/artifacts/_chats/<chatId>/`). The
//!   canonical checkout is the main checkout of the item's primary
//!   repository, never a worktree. `.zeron/` is excluded through the repo's
//!   `info/exclude`, never `.gitignore`.
//! - The registry row (kind [`KIND_ARTIFACTS`]) holds metadata only
//!   ([`ArtifactView`]); content snapshots live in the owning device's profile
//!   store, addressed by SHA-256. Other devices read content with the
//!   forwardable [`methods`] `ArtifactContent`.
//! - Agents publish through the `zeron-agent` MCP tools
//!   ([`crate::agent_tools::tools::ARTIFACTS`]); harnesses without tools link
//!   `[title](zeron-artifact:<path>)` and Zeron imports the file once.
//! - Render checks are served by a headed UI on the owning device: the UI
//!   subscribes to `WatchArtifactRenderRequests`, renders each
//!   [`ArtifactRenderRequest`] offscreen, and answers with
//!   `ArtifactRenderResult`.
//!
//! Wire conventions follow `work_outline.rs`: camelCase fields, every
//! non-identity field decodes when absent, snapshot enums are lenient.
//! Failures are strings `artifact/<code>: <message>`
//! ([`ArtifactError::from_wire_message`]).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::work_outline::{WorkOutlineActor, derive_id};

/// Registry row kind of artifact metadata rows.
pub const KIND_ARTIFACTS: &str = "artifacts";
/// The capability a device advertises when it serves the artifact RPCs and
/// the artifact agent tools.
pub const ARTIFACTS_CAPABILITY_V1: &str = "artifacts-v1";
/// Link scheme: `zeron-artifact:<artifactId>` or `zeron-artifact:<path>`.
pub const ARTIFACT_URL_SCHEME: &str = "zeron-artifact:";
/// Zeron's folder in a canonical checkout (listed in `info/exclude`).
pub const ZERON_DIR: &str = ".zeron";
/// Artifact root, relative to the canonical checkout.
pub const ARTIFACTS_DIR: &str = ".zeron/artifacts";
/// Folder of chats without a work item: `_chats/<chatId>`.
pub const CHATS_FOLDER: &str = "_chats";
/// Folder of artifacts registered with neither a work item nor a chat.
pub const SPACE_FOLDER: &str = "_space";
/// Where `ArtifactPromote` copies the current revision (relative to the
/// canonical checkout).
pub const PROMOTE_DIR: &str = "docs/plans";
/// The note of a revision Zeron recorded for an edit made outside
/// `artifact_publish`.
pub const EDITED_WITHOUT_NOTE: &str = "edited without a note";
/// The note of an imported file's first revision when none was given.
pub const IMPORTED_NOTE: &str = "imported";

/// Budgets (the registry op cap is 16 KiB: revisions are trimmed oldest
/// first until the row fits).
pub mod artifact_limits {
    /// Revisions kept on the row (newest first).
    pub const MAX_REVISIONS: usize = 50;
    /// Revision note, in Unicode scalar values (longer notes are cut).
    pub const MAX_NOTE_CHARS: usize = 200;
    /// Artifact title, in Unicode scalar values.
    pub const MAX_TITLE_CHARS: usize = 200;
    /// Role, in Unicode scalar values.
    pub const MAX_ROLE_CHARS: usize = 64;
    /// Largest file Zeron publishes or imports.
    pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
    /// Companion files of one `options` manifest.
    pub const MAX_COMPANIONS: usize = 16;
    /// Render checks per Run per minute.
    pub const RENDER_CHECKS_PER_MINUTE: usize = 20;
    /// How long the engine waits for the UI's render result.
    pub const RENDER_TIMEOUT_SECS: u64 = 30;
    /// Default render viewport.
    pub const DEFAULT_VIEWPORT_WIDTH: u32 = 1280;
    pub const DEFAULT_VIEWPORT_HEIGHT: u32 = 800;
    /// Viewport bounds a render request is clamped to.
    pub const MIN_VIEWPORT: u32 = 200;
    pub const MAX_VIEWPORT: u32 = 4096;
    /// Images (viewport × theme) one render check produces; the rest are
    /// dropped with a warning.
    pub const MAX_RENDER_IMAGES: usize = 4;
    /// Layout warnings (the probe's heuristics) one render check reports.
    pub const MAX_LAYOUT_WARNINGS: usize = 50;
    /// Longest `textSample` of a layout warning, in Unicode scalar values.
    pub const MAX_WARNING_TEXT_CHARS: usize = 80;
    /// Default viewports when a request names none: a narrow and a wide one.
    pub const DEFAULT_VIEWPORTS: [(u32, u32, &str); 2] =
        [(800, 800, "narrow"), (1440, 900, "wide")];
}

/// Known `role` values (the field is free-form).
pub mod artifact_roles {
    pub const PLAN: &str = "plan";
    pub const DIAGRAM: &str = "diagram";
    pub const STATE_MACHINE: &str = "state-machine";
    pub const COMPARISON: &str = "comparison";
    pub const DOC: &str = "doc";
}

// --------------------------------------------------------------------------
// Kinds
// --------------------------------------------------------------------------

/// How an artifact renders. camelCase on the wire; the agent tools also
/// accept the conventions' kebab spellings (`html-fragment`, `html-page`).
/// Lenient: an unknown kind reads as `unknown` (shown as plain content).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactKind {
    Markdown,
    /// Body content only; the host wraps it (theme, sandbox, CSP).
    #[serde(alias = "html-fragment", alias = "fragment")]
    HtmlFragment,
    /// A standalone page, rendered sandboxed.
    #[serde(alias = "html-page", alias = "html")]
    HtmlPage,
    Mermaid,
    Image,
    /// A manifest of option fragments (conventions section 6).
    Options,
    #[default]
    #[serde(other)]
    Unknown,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::HtmlFragment => "htmlFragment",
            Self::HtmlPage => "htmlPage",
            Self::Mermaid => "mermaid",
            Self::Image => "image",
            Self::Options => "options",
            Self::Unknown => "unknown",
        }
    }

    /// Strict parse of a requested kind (wire or kebab spelling); `None` for
    /// anything else.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "markdown" | "md" => Some(Self::Markdown),
            "htmlFragment" | "html-fragment" | "fragment" => Some(Self::HtmlFragment),
            "htmlPage" | "html-page" | "html" => Some(Self::HtmlPage),
            "mermaid" => Some(Self::Mermaid),
            "image" => Some(Self::Image),
            "options" => Some(Self::Options),
            _ => None,
        }
    }

    /// The kind a file name implies: `.fragment.html` / `.preview.html` →
    /// fragment, other `.html` → page, `.md` → markdown, `.mmd` /
    /// `.mermaid` → mermaid, images by extension, `.options.json` → options.
    /// Anything else is treated as Markdown text.
    pub fn from_path(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(crate::visualize::FRAGMENT_SUFFIX)
            || lower.ends_with(crate::visualize::PREVIEW_SUFFIX)
        {
            return Self::HtmlFragment;
        }
        if lower.ends_with(".options.json") {
            return Self::Options;
        }
        match lower.rsplit_once('.').map(|(_, ext)| ext) {
            Some("html" | "htm") => Self::HtmlPage,
            Some("mmd" | "mermaid") => Self::Mermaid,
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg") => Self::Image,
            _ => Self::Markdown,
        }
    }

    /// Text kinds travel as UTF-8; images as base64.
    pub fn is_binary(self) -> bool {
        matches!(self, Self::Image)
    }

    /// Visual kinds the conventions ask agents to render-check.
    pub fn is_visual(self) -> bool {
        matches!(
            self,
            Self::HtmlFragment | Self::HtmlPage | Self::Mermaid | Self::Image | Self::Options
        )
    }
}

/// MIME type for a stored file (by extension; text kinds default to
/// `text/plain`).
pub fn mime_type_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    match lower.rsplit_once('.').map(|(_, ext)| ext) {
        Some("md" | "markdown") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => "text/plain",
    }
}

// --------------------------------------------------------------------------
// Snapshot types
// --------------------------------------------------------------------------

/// One recorded revision of an artifact's content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRevision {
    /// Revision number: 1-based, monotonic per artifact (`r3` in the UI).
    pub id: u32,
    /// Lowercase hex SHA-256 of the content (the snapshot's store key).
    #[serde(default)]
    pub hash: String,
    /// What changed (the agent's `note`, [`EDITED_WITHOUT_NOTE`], or
    /// [`IMPORTED_NOTE`]).
    #[serde(default)]
    pub note: String,
    /// Epoch milliseconds.
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub by: WorkOutlineActor,
    #[serde(default)]
    pub size_bytes: u64,
    /// The publish operation that recorded it (idempotent retries return
    /// this revision). Internal; ≤ 64 chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

/// One artifact as the registry row holds it (metadata only). This is both
/// the row's field set and the snapshot type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactView {
    /// `derive_id` of (scope, title, role) — [`artifact_id`].
    pub id: String,
    pub space_id: String,
    /// The owning device (the canonical checkout's device): forward
    /// `ArtifactContent` / `ArtifactPromote` / `ArtifactRegister` there.
    #[serde(default)]
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// The Chat that first published it (set for every agent publish; the
    /// scope for chats without a work item).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Folder name under `.zeron/artifacts/` (`<slug>-<id8>` for a work
    /// item, `_chats/<chatId>` for a chat). Fixed at creation: renaming the
    /// item never moves files.
    #[serde(default)]
    pub folder: String,
    /// Absolute path of the canonical copy on the owning device.
    #[serde(default)]
    pub stored_path: String,
    /// Where the file was first published or imported from (provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_path: Option<String>,
    /// Stored standalone preview (`<name>.preview.html`) of a fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_path: Option<String>,
    /// Files copied alongside (an `options` manifest's fragments), relative
    /// to the stored file's folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companions: Vec<String>,
    /// Number of the newest revision (0 = none recorded).
    #[serde(default)]
    pub current_revision: u32,
    /// Newest first, at most [`artifact_limits::MAX_REVISIONS`].
    #[serde(default)]
    pub revisions: Vec<ArtifactRevision>,
    /// The work item's (or chat's) plan. At most one per scope.
    #[serde(default)]
    pub pinned_as_plan: bool,
    /// Answers given in Zeron to the plan's questions (Artifacts phase 3 D).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<ArtifactQuestionAnswer>,
    /// Comments on the rendered artifact (Artifacts phase 4 G). Never in the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ArtifactComment>,
    /// The option picked for an `options` artifact (phase 5 F).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picked: Option<ArtifactOptionPick>,
    /// Absolute path of the last promoted copy under `docs/plans/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_path: Option<String>,
    /// Epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

impl ArtifactView {
    /// `zeron-artifact:<id>`.
    pub fn url(&self) -> String {
        format!("{ARTIFACT_URL_SCHEME}{}", self.id)
    }

    /// The newest revision.
    pub fn current(&self) -> Option<&ArtifactRevision> {
        self.revision(self.current_revision)
    }

    /// A kept revision by number.
    pub fn revision(&self, id: u32) -> Option<&ArtifactRevision> {
        self.revisions.iter().find(|r| r.id == id)
    }

    /// The plan scope: the work item, else the chat.
    pub fn plan_scope(&self) -> Option<PlanScope<'_>> {
        if let Some(item) = self.work_item_id.as_deref() {
            return Some(PlanScope::WorkItem(item));
        }
        self.chat_id.as_deref().map(PlanScope::Chat)
    }
}

/// Where "the plan" is pinned: one per work item, or per chat without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanScope<'a> {
    WorkItem(&'a str),
    Chat(&'a str),
}

/// Params of `WatchArtifacts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchArtifactsParams {
    pub space_id: String,
}

/// One frame of `WatchArtifacts`: every live artifact of the Space.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactsSnapshot {
    pub space_id: String,
    /// Per-stream monotone frame counter.
    #[serde(default)]
    pub revision: u64,
    /// Sorted by `(workItemId, title, id)`; archived artifacts included
    /// (`archivedAt` set).
    #[serde(default)]
    pub artifacts: Vec<ArtifactView>,
}

impl ArtifactsSnapshot {
    pub fn artifact(&self, id: &str) -> Option<&ArtifactView> {
        self.artifacts.iter().find(|a| a.id == id)
    }

    /// Live artifacts of one work item, the pinned plan first.
    pub fn for_work_item(&self, work_item_id: &str) -> Vec<&ArtifactView> {
        let mut out: Vec<&ArtifactView> = self
            .artifacts
            .iter()
            .filter(|a| a.archived_at.is_none())
            .filter(|a| a.work_item_id.as_deref() == Some(work_item_id))
            .collect();
        out.sort_by_key(|a| !a.pinned_as_plan);
        out
    }

    /// Live artifacts first published by one chat.
    pub fn for_chat(&self, chat_id: &str) -> Vec<&ArtifactView> {
        self.artifacts
            .iter()
            .filter(|a| a.archived_at.is_none())
            .filter(|a| a.chat_id.as_deref() == Some(chat_id))
            .collect()
    }

    /// The pinned plan of a work item.
    pub fn plan_of(&self, work_item_id: &str) -> Option<&ArtifactView> {
        self.artifacts.iter().find(|a| {
            a.pinned_as_plan
                && a.archived_at.is_none()
                && a.work_item_id.as_deref() == Some(work_item_id)
        })
    }
}

// --------------------------------------------------------------------------
// RPC params and replies
// --------------------------------------------------------------------------

/// How `content` is encoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactContentEncoding {
    Base64,
    #[default]
    #[serde(other)]
    Utf8,
}

/// `ArtifactContent`: one revision's content. Forwardable: pass
/// `targetDeviceId = artifact.deviceId`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactContentParams {
    pub artifact_id: String,
    /// Revision number; `None` = the current stored file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
    /// A companion (`companions` entry, or the preview file name) instead of
    /// the main file. With `revision`, that revision's copy of the option
    /// file (revisions recorded before option files were snapshotted answer
    /// not-found).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactContentReply {
    pub artifact_id: String,
    #[serde(default)]
    pub kind: ArtifactKind,
    /// The revision served (the current one when none was asked; 0 when the
    /// current file has changed since the last recorded revision).
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub encoding: ArtifactContentEncoding,
    /// UTF-8 text, or base64 bytes for images.
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub size_bytes: u64,
    /// Reserved: the host wrapper for a fragment. Phase 1 engines leave it
    /// `None`; the UI wraps fragments with its own visualize wrapper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper_html: Option<String>,
}

/// `ArtifactRegister`: import an existing file (any path in the Space, or
/// in its canonical checkout) as revision 1, keeping `originPath`. A later
/// register of the same (scope, title, role) adds a revision when the
/// content changed. Forwardable (runs on the Space's device).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRegisterParams {
    pub space_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// Scope for a Space chat without a work item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Absolute, or relative to the Space folder.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ArtifactKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// Reply of `ArtifactRegister` (and the agent's `artifact_publish`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPublishReply {
    pub artifact_id: String,
    pub revision: u32,
    /// `zeron-artifact:<artifactId>`.
    pub url: String,
    pub stored_path: String,
    /// `false` when the content was unchanged (no new revision).
    #[serde(default)]
    pub changed: bool,
}

/// `ArtifactPromote`: copy the current revision to
/// `<canonical>/docs/plans/<file>` (uncommitted; an existing different file
/// is never overwritten: a `-2`, `-3`… suffix is added). Forwardable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPromoteParams {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPromoteReply {
    pub artifact_id: String,
    pub promoted_path: String,
}

/// `ArtifactPin`: pin (unpinning the scope's other plan) or unpin. A pure
/// registry write: local, any device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPinParams {
    pub artifact_id: String,
    pub pinned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPinReply {
    pub artifact_id: String,
    pub pinned: bool,
    /// Artifacts this call unpinned.
    #[serde(default)]
    pub unpinned: Vec<String>,
}

// --------------------------------------------------------------------------
// Render check (engine → UI)
// --------------------------------------------------------------------------

/// Render theme. Lenient: unknown reads as `system` (the user's current one).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactRenderTheme {
    Light,
    Dark,
    #[default]
    #[serde(other)]
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactViewport {
    pub width: u32,
    pub height: u32,
}

impl Default for ArtifactViewport {
    fn default() -> Self {
        Self {
            width: artifact_limits::DEFAULT_VIEWPORT_WIDTH,
            height: artifact_limits::DEFAULT_VIEWPORT_HEIGHT,
        }
    }
}

impl ArtifactViewport {
    /// Clamped to `[MIN_VIEWPORT, MAX_VIEWPORT]`.
    pub fn clamped(self) -> Self {
        let clamp = |v: u32| v.clamp(artifact_limits::MIN_VIEWPORT, artifact_limits::MAX_VIEWPORT);
        Self {
            width: clamp(self.width),
            height: clamp(self.height),
        }
    }
}

/// One viewport of a render check, optionally labelled (`narrow`, `wide`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderViewport {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ArtifactRenderViewport {
    /// Clamped to the viewport bounds; an empty label reads as none.
    pub fn clamped(self) -> Self {
        let size = ArtifactViewport {
            width: self.width,
            height: self.height,
        }
        .clamped();
        Self {
            width: size.width,
            height: size.height,
            label: self.label.filter(|l| !l.trim().is_empty()),
        }
    }

    pub fn size(&self) -> ArtifactViewport {
        ArtifactViewport {
            width: self.width,
            height: self.height,
        }
    }

    /// The label, else `WIDTHxHEIGHT`.
    pub fn display_label(&self) -> String {
        match &self.label {
            Some(label) if !label.trim().is_empty() => label.clone(),
            _ => format!("{}x{}", self.width, self.height),
        }
    }

    /// The default pair: 800 narrow and 1440 wide.
    pub fn defaults() -> Vec<Self> {
        artifact_limits::DEFAULT_VIEWPORTS
            .iter()
            .map(|(width, height, label)| Self {
                width: *width,
                height: *height,
                label: Some((*label).to_owned()),
            })
            .collect()
    }
}

/// The (viewport, theme) renders of one check: viewport-major, at most
/// [`artifact_limits::MAX_RENDER_IMAGES`]. Returns a warning when the cap
/// dropped renders. Empty `themes` reads as one system (current) theme.
pub fn plan_renders(
    viewports: &[ArtifactRenderViewport],
    themes: &[ArtifactRenderTheme],
) -> (
    Vec<(ArtifactRenderViewport, ArtifactRenderTheme)>,
    Option<String>,
) {
    let themes: Vec<ArtifactRenderTheme> = if themes.is_empty() {
        vec![ArtifactRenderTheme::System]
    } else {
        let mut unique = Vec::new();
        for theme in themes {
            if !unique.contains(theme) {
                unique.push(*theme);
            }
        }
        unique
    };
    let mut all = Vec::new();
    for viewport in viewports {
        for theme in &themes {
            all.push((viewport.clone(), *theme));
        }
    }
    let total = all.len();
    all.truncate(artifact_limits::MAX_RENDER_IMAGES);
    let warning = (total > all.len()).then(|| {
        format!(
            "Rendered {} of {total} requested viewport/theme combinations (at most {} images per \
             check); run the check again with fewer viewports or themes for the rest",
            all.len(),
            artifact_limits::MAX_RENDER_IMAGES
        )
    });
    (all, warning)
}

/// `WatchArtifactRenderRequests` params: the UI registers as render-capable
/// for as long as the stream stays open (IPC-only, never forwarded).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchArtifactRenderRequestsParams {
    /// Diagnostics label (`"zeron-ui"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

/// One item of `WatchArtifactRenderRequests`: render this offscreen exactly
/// as the artifact tab would (same wrapper, sandbox, theme) and answer with
/// `ArtifactRenderResult {requestId, …}` within
/// [`artifact_limits::RENDER_TIMEOUT_SECS`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderRequest {
    pub request_id: String,
    /// Set when checking a registered artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: ArtifactKind,
    /// The file rendered (stored path, or the agent's path).
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub encoding: ArtifactContentEncoding,
    #[serde(default)]
    pub content: String,
    /// For `options`: companion fragments, relative path → content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companions: Vec<ArtifactRenderCompanion>,
    /// Single viewport (older clients); the first of `viewports` when set.
    #[serde(default)]
    pub viewport: ArtifactViewport,
    #[serde(default)]
    pub theme: ArtifactRenderTheme,
    /// Viewports to render; empty reads as `[viewport]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub viewports: Vec<ArtifactRenderViewport>,
    /// Themes to render each viewport in; empty reads as `[theme]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<ArtifactRenderTheme>,
}

impl ArtifactRenderRequest {
    /// The viewports × themes this request renders (see [`plan_renders`]).
    pub fn renders(
        &self,
    ) -> (
        Vec<(ArtifactRenderViewport, ArtifactRenderTheme)>,
        Option<String>,
    ) {
        let viewports = if self.viewports.is_empty() {
            vec![
                ArtifactRenderViewport {
                    width: self.viewport.width,
                    height: self.viewport.height,
                    label: None,
                }
                .clamped(),
            ]
        } else {
            self.viewports
                .iter()
                .cloned()
                .map(ArtifactRenderViewport::clamped)
                .collect()
        };
        let themes = if self.themes.is_empty() {
            vec![self.theme]
        } else {
            self.themes.clone()
        };
        plan_renders(&viewports, &themes)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderCompanion {
    pub path: String,
    #[serde(default)]
    pub content: String,
}

/// What the UI found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderCheckResult {
    /// PNG of the viewport, base64 (no data: prefix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_png_base64: Option<String>,
    /// JS console errors, CSP violations, failed resources, Mermaid parse
    /// errors, empty render.
    #[serde(default)]
    pub errors: Vec<String>,
    /// Horizontal overflow, content outside the viewport, low contrast.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Every image rendered (viewport × theme); the first is also
    /// `screenshot_png_base64`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<ArtifactRenderScreenshot>,
    /// The probe's layout heuristics (clipped text, overlaps, narrow
    /// columns, children wider than their parent, fixed widths).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout_warnings: Vec<ArtifactLayoutWarning>,
}

/// One rendered image of a check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderScreenshot {
    /// The viewport's label.
    pub viewport: String,
    pub width: u32,
    pub height: u32,
    /// `light` or `dark` (the theme actually rendered).
    pub theme: String,
    /// PNG, base64 (no data: prefix).
    pub png_base64: String,
}

/// One layout-heuristic finding. A heuristic: always look at the screenshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactLayoutWarning {
    /// `clippedText`, `overlappingSiblings`, `narrowTextColumn`,
    /// `widerThanParent`, `fixedWidth`.
    pub kind: String,
    pub selector: String,
    /// At most [`artifact_limits::MAX_WARNING_TEXT_CHARS`] characters.
    pub text_sample: String,
    /// The viewport label it was found at.
    pub viewport: String,
    pub theme: String,
}

/// `ArtifactRenderResult` params (UI → engine, IPC-only). Exactly one of
/// `result` / `error`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderResultParams {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ArtifactRenderCheckResult>,
    /// The UI could not render at all (reported to the agent as
    /// `artifact/renderFailed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRenderResultReply {
    /// `false` when the request was unknown or already timed out.
    pub accepted: bool,
}

// --------------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------------

/// Error codes (`artifact/<code>`). Lenient: unknown reads as `internal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactErrorCode {
    NotFound,
    Invalid,
    /// The caller may not touch this artifact (agent scope).
    Forbidden,
    /// The source file is on the deny-list (credentials, `.env`, oversize,
    /// not a regular file).
    Denied,
    TooLarge,
    /// The Space lives on another device (call it there).
    WrongDevice,
    /// No headed Zeron window on the owning device can render.
    RenderUnavailable,
    /// The UI did not answer within the timeout.
    RenderTimeout,
    /// The UI could not render.
    RenderFailed,
    /// Too many render checks this minute.
    RateLimited,
    /// Filesystem or git failure.
    Io,
    #[serde(other)]
    Internal,
}

impl ArtifactErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "notFound",
            Self::Invalid => "invalid",
            Self::Forbidden => "forbidden",
            Self::Denied => "denied",
            Self::TooLarge => "tooLarge",
            Self::WrongDevice => "wrongDevice",
            Self::RenderUnavailable => "renderUnavailable",
            Self::RenderTimeout => "renderTimeout",
            Self::RenderFailed => "renderFailed",
            Self::RateLimited => "rateLimited",
            Self::Io => "io",
            Self::Internal => "internal",
        }
    }

    fn parse(code: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(code.to_owned())).unwrap_or(Self::Internal)
    }
}

/// Prefix of a wire-encoded [`ArtifactError`].
pub const ARTIFACT_ERROR_PREFIX: &str = "artifact/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    pub code: ArtifactErrorCode,
    pub message: String,
}

impl ArtifactError {
    pub fn new(code: ArtifactErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ArtifactErrorCode::NotFound, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ArtifactErrorCode::Invalid, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ArtifactErrorCode::Forbidden, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ArtifactErrorCode::Io, message)
    }

    /// `artifact/<code>: <message>`.
    pub fn to_wire_message(&self) -> String {
        format!(
            "{ARTIFACT_ERROR_PREFIX}{}: {}",
            self.code.as_str(),
            self.message
        )
    }

    /// Inverse of [`Self::to_wire_message`] (also finds it inside a longer
    /// transport error string). `None` when no `artifact/` prefix is present.
    pub fn from_wire_message(text: &str) -> Option<Self> {
        let start = text.find(ARTIFACT_ERROR_PREFIX)?;
        let rest = &text[start + ARTIFACT_ERROR_PREFIX.len()..];
        let (code, message) = rest.split_once(':').unwrap_or((rest, ""));
        Some(Self::new(
            ArtifactErrorCode::parse(code.trim()),
            message.trim(),
        ))
    }
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_wire_message())
    }
}

impl std::error::Error for ArtifactError {}

// --------------------------------------------------------------------------
// Pure helpers
// --------------------------------------------------------------------------

/// Title as compared for identity: trimmed, whitespace collapsed, lowercased.
pub fn normalize_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Display title: whitespace collapsed, cut to `MAX_TITLE_CHARS`.
pub fn clean_title(title: &str) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&title, artifact_limits::MAX_TITLE_CHARS)
}

/// Note: one line, cut to `MAX_NOTE_CHARS`.
pub fn clean_note(note: &str) -> String {
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&note, artifact_limits::MAX_NOTE_CHARS)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The artifact identity: same scope (work item, else chat, else Space),
/// same normalized title, same role → same id (32 hex).
pub fn artifact_id(
    space_id: &str,
    work_item_id: Option<&str>,
    chat_id: Option<&str>,
    title: &str,
    role: Option<&str>,
) -> String {
    let scope = match (work_item_id, chat_id) {
        (Some(item), _) => format!("item:{item}"),
        (None, Some(chat)) => format!("chat:{chat}"),
        (None, None) => format!("space:{space_id}"),
    };
    let role = role.map(str::trim).unwrap_or_default().to_lowercase();
    derive_id(
        &format!("{scope}\u{0}{}\u{0}{role}", normalize_title(title)),
        "artifact",
    )
}

/// `artifact-<32 hex>` (41 chars, under the 64-char operation id cap): the
/// operation of an agent publish, from the token Chat and its key.
pub fn publish_operation_id(chat_id: &str, idempotency_key: &str) -> String {
    format!(
        "artifact-{}",
        derive_id(
            &format!("{chat_id}\u{0}{idempotency_key}"),
            "artifact-publish"
        )
    )
}

/// Lowercase hex SHA-256.
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A work item's folder name: sanitized title (ASCII alphanumerics and `-`,
/// at most 48 chars) + `-` + the first 8 characters of the item id.
pub fn work_item_folder(title: &str, work_item_id: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !slug.is_empty() {
            slug.push('-');
            dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let short: String = work_item_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(8)
        .collect();
    let short = if short.is_empty() { "item" } else { &short };
    if slug.is_empty() {
        short.to_owned()
    } else {
        format!("{slug}-{short}")
    }
}

/// A chat's folder: `_chats/<chatId>` (the id sanitized like a path segment).
pub fn chat_folder(chat_id: &str) -> String {
    let id: String = chat_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{CHATS_FOLDER}/{}", id.trim_matches('.'))
}

/// `name` with `-tag` inserted before its compound suffix
/// (`.fragment.html`, `.preview.html`, `.options.json`) or extension.
pub fn tagged_file_name(name: &str, tag: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let suffix_len = [
        crate::visualize::FRAGMENT_SUFFIX,
        crate::visualize::PREVIEW_SUFFIX,
        ".options.json",
    ]
    .iter()
    .find(|s| lower.ends_with(**s) && name.len() > s.len())
    .map(|s| s.len())
    .or_else(|| {
        name.rfind('.')
            .filter(|&dot| dot > 0)
            .map(|dot| name.len() - dot)
    })
    .unwrap_or(0);
    let (stem, suffix) = name.split_at(name.len() - suffix_len);
    format!("{stem}-{tag}{suffix}")
}

/// One `[label](zeron-artifact:<target>)` link in assistant text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactLink {
    pub label: String,
    /// An artifact id or a path (absolute, or relative to the chat's cwd).
    pub target: String,
}

impl ArtifactLink {
    /// Whether the target names an artifact id (32 lowercase hex) rather
    /// than a path.
    pub fn is_id(&self) -> bool {
        is_artifact_id(&self.target)
    }
}

/// 32 lowercase hex characters.
pub fn is_artifact_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Every Markdown link whose target uses the `zeron-artifact:` scheme, in
/// order, de-duplicated by target. Targets may be wrapped in `<…>`; a link
/// title (`"…"`) after the target is ignored.
pub fn artifact_links(text: &str) -> Vec<ArtifactLink> {
    let mut out: Vec<ArtifactLink> = Vec::new();
    let needle = format!("]({ARTIFACT_URL_SCHEME}");
    let mut from = 0;
    while let Some(rel) = text[from..].find(&needle) {
        let close = from + rel;
        let target_start = close + needle.len();
        from = target_start;
        // The label: back to the matching `[` on the same line.
        let before = &text[..close];
        let Some(open) = before.rfind('[') else {
            continue;
        };
        if before[open..].contains('\n') {
            continue;
        }
        let label = before[open + 1..].trim().to_owned();
        let rest = &text[target_start..];
        let Some(end) = rest.find(')') else {
            continue;
        };
        let mut target = rest[..end].trim();
        if let Some((path, _title)) = target.split_once(" \"") {
            target = path.trim();
        }
        let target = target.trim_start_matches('<').trim_end_matches('>').trim();
        if target.is_empty() || target.contains('\n') || target.chars().any(char::is_control) {
            continue;
        }
        let target = percent_decode(target);
        if !out.iter().any(|l| l.target == target) {
            out.push(ArtifactLink { label, target });
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// An `options` manifest (conventions section 6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionsManifest {
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default)]
    pub options: Vec<OptionsManifestEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionsManifestEntry {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// Relative to the manifest.
    pub path: String,
}

impl OptionsManifest {
    /// `question` when it is a plan question id (`Q-3`), upper-cased: the
    /// question a pick answers.
    pub fn linked_question_id(&self) -> Option<String> {
        let id = self.question.as_deref()?.trim().to_ascii_uppercase();
        let digits = id.strip_prefix("Q-")?;
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(id)
    }

    /// Parse and validate: every option path relative, without `..`, at most
    /// `MAX_COMPANIONS` options.
    pub fn parse(text: &str) -> Result<Self, ArtifactError> {
        let manifest: Self = serde_json::from_str(text)
            .map_err(|e| ArtifactError::invalid(format!("options manifest: {e}")))?;
        if manifest.options.is_empty() {
            return Err(ArtifactError::invalid("options manifest lists no options"));
        }
        if manifest.options.len() > artifact_limits::MAX_COMPANIONS {
            return Err(ArtifactError::invalid(format!(
                "options manifests are limited to {} options",
                artifact_limits::MAX_COMPANIONS
            )));
        }
        for option in &manifest.options {
            if !is_safe_relative_path(&option.path) {
                return Err(ArtifactError::invalid(format!(
                    "option {:?}: path must be relative to the manifest, without `..`",
                    option.id
                )));
            }
        }
        Ok(manifest)
    }
}

/// A relative path that stays inside its base (no root, no `..`, no empty
/// or `.` segments, no backslashes).
pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Open questions (conventions section 4): `> [!question] Q-n` blocks whose
/// `status:` is not `resolved` (absent = open), plus HTML
/// `data-zeron-question` elements without `data-status="resolved"`.
/// Phase 1 counts; the questions panel (phase 3) parses fully.
pub fn count_open_questions(text: &str) -> u32 {
    let mut count = 0u32;
    let mut in_block = false;
    let mut resolved = false;
    let close = |in_block: &mut bool, resolved: &mut bool, count: &mut u32| {
        if *in_block && !*resolved {
            *count += 1;
        }
        *in_block = false;
        *resolved = false;
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('>') {
            let rest = rest.trim();
            if rest.to_ascii_lowercase().starts_with("[!question]") {
                close(&mut in_block, &mut resolved, &mut count);
                in_block = true;
                continue;
            }
            if in_block && let Some(status) = rest.strip_prefix("status:") {
                resolved = status.trim().eq_ignore_ascii_case("resolved");
            }
            continue;
        }
        close(&mut in_block, &mut resolved, &mut count);
    }
    close(&mut in_block, &mut resolved, &mut count);
    // HTML questions.
    let mut from = 0;
    while let Some(rel) = text[from..].find("data-zeron-question=") {
        let start = from + rel;
        let tag_end = text[start..].find('>').map_or(text.len(), |e| start + e);
        let tag = &text[start..tag_end];
        if !tag.contains("data-status=\"resolved\"") {
            count += 1;
        }
        from = tag_end.max(start + 1);
    }
    count
}

/// Unverified claims (conventions section 5): `{unverified}` markers,
/// Markdown links titled `"unverified"`, and HTML `data-claim="unverified"`.
pub fn count_unverified_claims(text: &str) -> u32 {
    (text.matches("{unverified}").count()
        + text.matches(" \"unverified\")").count()
        + text.matches("data-claim=\"unverified\"").count()) as u32
}

mod comments;
pub use comments::*;
mod picks;
pub use picks::*;
mod questions;
pub use questions::*;

#[cfg(test)]
mod tests;
