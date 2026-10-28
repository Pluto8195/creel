//! Picking one option of an `options` artifact (Artifacts phase 5 F).
//!
//! The pick lives on the artifact row (`ArtifactView.picked`), written alone
//! like question answers: the manifest and its fragments are never edited.

use serde::{Deserialize, Serialize};

use crate::work_outline::WorkOutlineActor;

/// Longest option label / note kept on a pick, in Unicode scalar values.
pub const MAX_PICK_CHARS: usize = 400;
/// Longest option id kept on a pick.
pub const MAX_PICK_ID_CHARS: usize = 80;

/// The option Mikey picked for an options artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactOptionPick {
    /// The manifest entry's `id`.
    pub option_id: String,
    /// The entry's label when it was picked.
    #[serde(default)]
    pub label: String,
    /// Optional free-text note that went with the pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Epoch milliseconds.
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub by: WorkOutlineActor,
    /// The revision that was current when it was picked.
    #[serde(default)]
    pub revision: u32,
}

/// `ArtifactPickOption`: pick (or, with no `optionId`, un-pick) an option.
/// A registry write like `ArtifactPin`: local, not forwardable. The UI then
/// messages the publishing chat, as it does for answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPickOptionParams {
    pub artifact_id: String,
    /// `None` / empty clears the pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    /// The option's label (the UI has the manifest; the engine does not
    /// need the owning device's files to record a pick).
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPickOptionReply {
    pub artifact_id: String,
    /// The recorded pick, `None` after an un-pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picked: Option<ArtifactOptionPick>,
}

/// The message the publishing chat gets: `Picked option C for "<title>":
/// <label>` (+ the note), or the change / un-pick variants.
pub fn pick_message(
    title: &str,
    previous: Option<&ArtifactOptionPick>,
    now: Option<&ArtifactOptionPick>,
) -> String {
    match now {
        Some(pick) => {
            let label = if pick.label.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", pick.label.trim())
            };
            let verb = match previous {
                Some(prev) if prev.option_id != pick.option_id => "Changed my pick to option",
                Some(_) => "Still picking option",
                None => "Picked option",
            };
            let mut text = format!("{verb} {} for \"{}\"{label}", pick.option_id, title.trim());
            if let Some(note) = pick.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                text.push_str(&format!("\n\nNote: {note}"));
            }
            text
        }
        None => match previous {
            Some(prev) => format!(
                "Withdrew my pick of option {} for \"{}\": I haven't chosen yet.",
                prev.option_id,
                title.trim()
            ),
            None => format!("No option picked for \"{}\".", title.trim()),
        },
    }
}
