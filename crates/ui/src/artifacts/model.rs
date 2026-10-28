//! Pure presentation logic for artifacts: link parsing and resolution, the
//! header line, row labels, and which component renders a content reply.
//! GUI-free and unit tested.
use mast_proto::artifacts::{
    ALL_ARTIFACTS_DIRS, EDITED_WITHOUT_NOTE, OptionsManifest, is_artifact_id, strip_artifact_scheme,
};
use mast_proto::{ArtifactContentEncoding, ArtifactContentReply, ArtifactKind, ArtifactView};

use super::store::ArtifactStore;

/// The target of a `mast-artifact:` (or legacy `zeron-artifact:`) link
/// (`None` for any other link).
pub(crate) fn link_target(url: &str) -> Option<&str> {
    let target = strip_artifact_scheme(url.trim())?.trim();
    let target = target.trim_start_matches('<').trim_end_matches('>');
    (!target.is_empty()).then_some(target)
}

/// What clicking a `mast-artifact:` link does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkResolution {
    /// Open this registered artifact.
    Open(String),
    /// An id that no watched Space knows (yet): open it anyway; the tab
    /// shows "not found" until a frame names it.
    UnknownId(String),
    /// A path no artifact covers: register it, then open the result.
    Register(String),
}

/// Resolve a link target against the watched snapshots. `cwd` is the
/// linking chat's checkout (relative paths), `space_id` its Space.
pub(crate) fn resolve_link(
    target: &str,
    store: &ArtifactStore,
    cwd: Option<&str>,
    space_id: Option<&str>,
) -> LinkResolution {
    let target = percent_decoded(target);
    if is_artifact_id(&target) {
        return match store.artifact(&target) {
            Some(artifact) => LinkResolution::Open(artifact.id.clone()),
            None => LinkResolution::UnknownId(target),
        };
    }
    match store.artifact_by_path(&target, cwd, space_id) {
        Some(artifact) => LinkResolution::Open(artifact.id.clone()),
        None => LinkResolution::Register(target),
    }
}

fn percent_decoded(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// The kind's glyph.
pub(crate) fn kind_icon(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Markdown => crate::icons::FILE_MARKDOWN,
        ArtifactKind::HtmlFragment | ArtifactKind::HtmlPage => crate::icons::FILE_CODE,
        ArtifactKind::Mermaid => crate::icons::WIDGET,
        ArtifactKind::Image => crate::icons::FILE_IMAGE,
        ArtifactKind::Options => crate::icons::SPLIT_COLUMNS,
        ArtifactKind::Unknown => crate::icons::DOCUMENT,
    }
}

pub(crate) fn kind_label(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Markdown => "Markdown",
        ArtifactKind::HtmlFragment => "HTML fragment",
        ArtifactKind::HtmlPage => "HTML page",
        ArtifactKind::Mermaid => "Mermaid",
        ArtifactKind::Image => "Image",
        ArtifactKind::Options => "Options",
        ArtifactKind::Unknown => "File",
    }
}

/// "just now", "5m", "3h", "2d", "6w" — compact age of `at` at `now`
/// (both epoch ms).
pub(crate) fn age_label(now: i64, at: i64) -> String {
    let seconds = (now - at).max(0) / 1000;
    match seconds {
        s if s < 60 => "just now".to_owned(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 86_400 * 14 => format!("{}d", s / 86_400),
        s => format!("{}w", s / (86_400 * 7)),
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The Artifact tab header for the revision on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderModel {
    pub(crate) title: String,
    pub(crate) role: Option<String>,
    /// The revision shown (`0` = the current file, changed since its last
    /// recorded revision).
    pub(crate) revision: u32,
    pub(crate) current: bool,
    pub(crate) note: Option<String>,
    /// The shown revision was recorded by the watcher for an edit made
    /// outside `artifact_publish`.
    pub(crate) edited_without_note: bool,
    pub(crate) pinned: bool,
    pub(crate) promoted_path: Option<String>,
}

pub(crate) fn header(artifact: &ArtifactView, shown: Option<u32>) -> HeaderModel {
    let revision = shown.unwrap_or(artifact.current_revision);
    let recorded = artifact.revision(revision);
    let note = recorded
        .map(|r| r.note.trim().to_owned())
        .filter(|note| !note.is_empty());
    HeaderModel {
        title: artifact.title.clone(),
        role: artifact.role.clone().filter(|role| !role.trim().is_empty()),
        revision,
        current: revision == artifact.current_revision,
        edited_without_note: note.as_deref() == Some(EDITED_WITHOUT_NOTE),
        note,
        pinned: artifact.pinned_as_plan,
        promoted_path: artifact.promoted_path.clone(),
    }
}

/// "r3 · tightened rollout" / "r3".
pub(crate) fn revision_label(revision: u32, note: Option<&str>) -> String {
    match note {
        Some(note) if !note.is_empty() => format!("r{revision} \u{b7} {note}"),
        _ => format!("r{revision}"),
    }
}

/// A row of the Details Artifacts section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactRow {
    pub(crate) id: String,
    pub(crate) icon: &'static str,
    pub(crate) title: String,
    pub(crate) role: Option<String>,
    pub(crate) revision: String,
    pub(crate) age: String,
    pub(crate) pinned: bool,
}

pub(crate) fn row(artifact: &ArtifactView, now: i64) -> ArtifactRow {
    ArtifactRow {
        id: artifact.id.clone(),
        icon: kind_icon(artifact.kind),
        title: artifact.title.clone(),
        role: artifact.role.clone().filter(|role| !role.trim().is_empty()),
        revision: format!("r{}", artifact.current_revision),
        age: age_label(now, artifact.updated_at),
        pinned: artifact.pinned_as_plan,
    }
}

/// How the tab presents a loaded content reply.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ArtifactBody {
    /// The native Markdown preview.
    Markdown(String),
    /// A web document in the sandboxed web view (fragment wrapper, page
    /// sandbox, or the Mermaid wrapper).
    Web { kind: WebKind, source: String },
    /// A native image.
    Image { bytes: Vec<u8>, mime_type: String },
    /// Phase 1: the options listed with links (side by side is phase 5).
    Options(OptionsManifest),
    /// Plain text (unknown kinds).
    Text(String),
    /// The content could not be presented.
    Invalid(String),
}

/// Which wrapper a web body uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebKind {
    Fragment,
    Page,
    Mermaid,
    /// An image shown in the web wrapper (render checks only).
    Image,
}

/// The component a kind renders with. Pure.
pub(crate) fn body_for(kind: ArtifactKind, reply: &ArtifactContentReply) -> ArtifactBody {
    let text = || match reply.encoding {
        ArtifactContentEncoding::Utf8 => Ok(reply.content.clone()),
        ArtifactContentEncoding::Base64 => decode_base64(&reply.content)
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_owned())),
    };
    let kind = if kind == ArtifactKind::Unknown {
        reply.kind
    } else {
        kind
    };
    match kind {
        ArtifactKind::Image => {
            let bytes = match reply.encoding {
                ArtifactContentEncoding::Base64 => decode_base64(&reply.content),
                ArtifactContentEncoding::Utf8 => Ok(reply.content.clone().into_bytes()),
            };
            match bytes {
                Ok(bytes) => ArtifactBody::Image {
                    bytes,
                    mime_type: reply.mime_type.clone(),
                },
                Err(error) => {
                    ArtifactBody::Invalid(format!("Couldn\u{2019}t read the image ({error})."))
                }
            }
        }
        kind => match text() {
            Err(error) => {
                ArtifactBody::Invalid(format!("Couldn\u{2019}t read the artifact ({error})."))
            }
            Ok(text) => match kind {
                ArtifactKind::Markdown => ArtifactBody::Markdown(text),
                ArtifactKind::HtmlFragment => ArtifactBody::Web {
                    kind: WebKind::Fragment,
                    source: text,
                },
                ArtifactKind::HtmlPage => ArtifactBody::Web {
                    kind: WebKind::Page,
                    source: text,
                },
                ArtifactKind::Mermaid => ArtifactBody::Web {
                    kind: WebKind::Mermaid,
                    source: text,
                },
                ArtifactKind::Options => match OptionsManifest::parse(&text) {
                    Ok(manifest) => ArtifactBody::Options(manifest),
                    Err(error) => ArtifactBody::Invalid(error.message),
                },
                ArtifactKind::Image | ArtifactKind::Unknown => ArtifactBody::Text(text),
            },
        },
    }
}

pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|error| error.to_string())
}

/// The text the source view and the revision diff compare (`None` for
/// binary content).
pub(crate) fn source_text(reply: &ArtifactContentReply) -> Option<String> {
    match reply.encoding {
        ArtifactContentEncoding::Utf8 => Some(reply.content.clone()),
        ArtifactContentEncoding::Base64 if reply.kind == ArtifactKind::Image => None,
        ArtifactContentEncoding::Base64 => decode_base64(&reply.content)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok()),
    }
}

/// The checkout-relative path Markdown links resolve against:
/// `.mast/artifacts/<folder>/<file>` (or the legacy `.zeron/artifacts/...`;
/// so `mast-file:` and repo-relative links resolve from the canonical
/// checkout's root), else the file name.
pub(crate) fn markdown_document_path(stored_path: &str) -> String {
    match artifacts_marker(stored_path) {
        Some(at) => stored_path[at + 1..].to_owned(),
        None => stored_path
            .rsplit('/')
            .next()
            .unwrap_or(stored_path)
            .to_owned(),
    }
}

/// The canonical checkout root of a stored path (the part before
/// `/.mast/artifacts/`, or the legacy `/.zeron/artifacts/`).
pub(crate) fn canonical_root(stored_path: &str) -> Option<&str> {
    artifacts_marker(stored_path).map(|at| &stored_path[..at])
}

/// Byte offset of the leading `/` of the first `/<artifacts dir>/` marker
/// (new or legacy root) in `stored_path`.
fn artifacts_marker(stored_path: &str) -> Option<usize> {
    ALL_ARTIFACTS_DIRS
        .iter()
        .filter_map(|dir| stored_path.find(&format!("/{dir}/")))
        .min()
}
