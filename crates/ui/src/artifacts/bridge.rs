//! The frame → host message bridge (Artifacts phase 4).
//!
//! Until now the sandboxed artifact frame could only be *driven* (find, zoom)
//! and read by polling; a card or a selection inside the page could not tell
//! the app anything except "a link was clicked". This is the general channel:
//!
//! ```text
//! frame ─postMessage({mastMsg})→ outer doc (HOST_SCRIPT)
//!       ─click on hidden <a href="mast-msg:<JSON>">→ native delegate
//!       (browser::document_view) ─BrowserEvent::DocumentMessage→ ArtifactTab
//! ```
//!
//! It rides the same user-activated-navigation trick as the link bridge, so
//! it needs no WebKit script-message handler and works under the frame's
//! `sandbox="allow-scripts"` + CSP. Every message is untrusted page input:
//! [`FrameMessage::parse`] accepts a closed set of shapes, with size limits,
//! and ignores everything else. Host → frame commands use
//! [`frame_command_script`] (the `__mastFrameCmd` entry point of
//! `find::HOST_SCRIPT`).

use serde_json::Value;
use mast_proto::CommentAnchor;
use mast_proto::artifacts::{MAX_COMMENT_CHARS, MAX_QUOTE_CHARS};

/// In the outer document: relays the frame's `{mastMsg}` to the native
/// delegate as a `mast-msg:` navigation. Messages from any other window,
/// non-objects and anything over 16 KiB of JSON are dropped.
pub(crate) const HOST_SCRIPT: &str = r##"window.addEventListener("message", (event) => {
  const frame = document.querySelector("iframe");
  if (!frame || event.source !== frame.contentWindow) return;
  const msg = event.data && event.data.mastMsg;
  if (!msg || typeof msg !== "object") return;
  let json;
  try { json = JSON.stringify(msg); } catch (_) { return; }
  if (!json || json.length > 12000) return;
  const a = document.createElement("a");
  a.href = "mast-msg:" + encodeURIComponent(json);
  a.rel = "noreferrer";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
});"##;

/// What a frame may tell the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameMessage {
    /// The page finished loading and its scripts are listening.
    Ready,
    /// The reader selected text (a comment can be started on it).
    Selection {
        anchor: CommentAnchor,
        quote: String,
    },
    /// The selection was cleared.
    SelectionCleared,
    /// The reader picked an element in comment mode.
    Draft {
        anchor: CommentAnchor,
        quote: String,
    },
    /// The reader clicked a comment marker.
    FocusComment(String),
    /// The reader answered a plan question card in place.
    Answer { id: String, text: String },
}

impl FrameMessage {
    /// Parse and validate one bridged message. `None` for anything that is
    /// not exactly a known shape.
    pub(crate) fn parse(json: &str) -> Option<FrameMessage> {
        let value: Value = serde_json::from_str(json).ok()?;
        let object = value.as_object()?;
        let text = |key: &str, max: usize| -> Option<String> {
            let value = object.get(key)?.as_str()?;
            Some(value.chars().take(max).collect())
        };
        match object.get("t")?.as_str()? {
            "ready" => Some(FrameMessage::Ready),
            "selection-clear" => Some(FrameMessage::SelectionCleared),
            kind @ ("selection" | "draft") => {
                let anchor = parse_anchor(object.get("anchor")?)?;
                let quote = text("quote", MAX_QUOTE_CHARS).unwrap_or_default();
                let quote = collapse(&quote);
                Some(if kind == "draft" {
                    FrameMessage::Draft { anchor, quote }
                } else {
                    FrameMessage::Selection { anchor, quote }
                })
            }
            "focus" => {
                let id = mast_proto::artifacts::normalize_comment_id(&text("id", 16)?)?;
                Some(FrameMessage::FocusComment(id))
            }
            "answer" => {
                let id = text("id", 16)?.trim().to_ascii_uppercase();
                let valid = id
                    .strip_prefix("Q-")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                let answer = text("text", MAX_COMMENT_CHARS)?.trim().to_owned();
                (valid && !answer.is_empty()).then_some(FrameMessage::Answer { id, text: answer })
            }
            _ => None,
        }
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_anchor(value: &Value) -> Option<CommentAnchor> {
    let object = value.as_object()?;
    let field = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default()
    };
    let optional = |key: &str| Some(field(key)).filter(|v| !v.trim().is_empty());
    let anchor = CommentAnchor {
        id: optional("id"),
        selector: optional("selector"),
        prefix: field("prefix"),
        exact: field("exact"),
        suffix: field("suffix"),
    }
    .cleaned();
    (!anchor.is_empty()).then_some(anchor)
}

/// The script that sends `message` to the frame through the outer document.
pub(crate) fn frame_command_script(message: &Value) -> String {
    // JSON is valid JS except U+2028 / U+2029 in old engines; escape them.
    let json = message
        .to_string()
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!("window.__mastFrameCmd&&window.__mastFrameCmd({json});")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_messages_parse_with_limits() {
        assert_eq!(
            FrameMessage::parse(r#"{"t":"ready"}"#),
            Some(FrameMessage::Ready)
        );
        assert_eq!(
            FrameMessage::parse(r#"{"t":"selection-clear"}"#),
            Some(FrameMessage::SelectionCleared)
        );
        let draft = json!({
            "t": "draft",
            "anchor": { "id": " intake ", "exact": "Receiving  intake", "prefix": "", "suffix": "x" },
            "quote": "Receiving\n intake",
        });
        let Some(FrameMessage::Draft { anchor, quote }) = FrameMessage::parse(&draft.to_string())
        else {
            panic!("draft");
        };
        assert_eq!(anchor.id.as_deref(), Some("intake"));
        assert_eq!(anchor.exact, "Receiving intake");
        assert_eq!(quote, "Receiving intake");
        assert_eq!(
            FrameMessage::parse(r#"{"t":"focus","id":"c-4"}"#),
            Some(FrameMessage::FocusComment("C-4".into()))
        );
        assert_eq!(
            FrameMessage::parse(r#"{"t":"answer","id":"q-3","text":"  Use MPN  "}"#),
            Some(FrameMessage::Answer {
                id: "Q-3".into(),
                text: "Use MPN".into()
            })
        );
    }

    #[test]
    fn malformed_or_hostile_messages_are_ignored() {
        for bad in [
            "",
            "null",
            "[]",
            r#"{"t":"unknown"}"#,
            r#"{"t":"selection"}"#,
            r#"{"t":"selection","anchor":{}}"#,
            r#"{"t":"selection","anchor":{"id":"  "}}"#,
            r#"{"t":"focus","id":"Q-1"}"#,
            r#"{"t":"focus","id":7}"#,
            r#"{"t":"answer","id":"Q-1","text":"  "}"#,
            r#"{"t":"answer","id":"X-1","text":"hi"}"#,
            r#"{"t":"answer","id":"Q-","text":"hi"}"#,
            r#"{"t":"answer","id":"Q-1; drop","text":"hi"}"#,
        ] {
            assert_eq!(FrameMessage::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn long_text_is_clamped() {
        let long = "x".repeat(5000);
        let json = json!({ "t": "answer", "id": "Q-1", "text": long }).to_string();
        let Some(FrameMessage::Answer { text, .. }) = FrameMessage::parse(&json) else {
            panic!("answer");
        };
        assert_eq!(text.chars().count(), MAX_COMMENT_CHARS);
        let json =
            json!({ "t": "selection", "anchor": { "exact": long }, "quote": long }).to_string();
        let Some(FrameMessage::Selection { anchor, quote }) = FrameMessage::parse(&json) else {
            panic!("selection");
        };
        assert!(anchor.exact.chars().count() <= MAX_QUOTE_CHARS);
        assert!(quote.chars().count() <= MAX_QUOTE_CHARS);
    }

    #[test]
    fn command_scripts_escape_line_separators() {
        let script = frame_command_script(&json!({ "mastCommentMode": true, "x": "a\u{2028}b" }));
        assert!(script.starts_with("window.__mastFrameCmd&&"));
        assert!(!script.contains('\u{2028}'));
        assert!(script.contains("\\u2028"));
    }

    #[test]
    fn host_script_checks_the_source_and_the_size() {
        assert!(HOST_SCRIPT.contains("event.source !== frame.contentWindow"));
        assert!(HOST_SCRIPT.contains("mast-msg:"));
        assert!(HOST_SCRIPT.contains("12000"));
    }
}
