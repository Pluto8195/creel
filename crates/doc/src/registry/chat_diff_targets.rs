//! `chatDiffTargets` rows: the per-chat checkouts the Changes pane diffs
//! besides the chat's own `cwd` checkout (contract § diff-target amendment).
//! One row per chat, id = chat id. Written compare-before-write; deleted with
//! the chat.

use super::*;
use zeron_proto::{
    ChatDiffTargetsRow, MAX_CHAT_DIFF_TARGET_OPS, MAX_CHAT_DIFF_TARGETS, StoredChatDiffTarget,
};

/// Wire version of the row's JSON payload.
const ROW_VERSION: i64 = 1;

/// Decode one row; `None` (and a warning) when malformed.
fn decode(row: &RegistryRow) -> Option<ChatDiffTargetsRow> {
    let chat_id = row
        .fields
        .get("chatId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?
        .to_owned();
    let decoded = (|| -> Result<ChatDiffTargetsRow, serde_json::Error> {
        let targets: Vec<StoredChatDiffTarget> = match row.fields.get("targets") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(text)) => serde_json::from_str(text)?,
            Some(other) => serde_json::from_value(other.clone())?,
        };
        let ops: Vec<String> = match row.fields.get("ops") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(text)) => serde_json::from_str(text)?,
            Some(other) => serde_json::from_value(other.clone())?,
        };
        Ok(ChatDiffTargetsRow {
            chat_id: chat_id.clone(),
            targets,
            selected: row
                .fields
                .get("selected")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ops,
            updated_at: row
                .fields
                .get("updatedAt")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        })
    })();
    match decoded {
        Ok(mut value) => {
            value.targets.truncate(MAX_CHAT_DIFF_TARGETS);
            let over = value.ops.len().saturating_sub(MAX_CHAT_DIFF_TARGET_OPS);
            value.ops.drain(..over);
            Some(value)
        }
        Err(err) => {
            tracing::warn!(kind = %row.kind, row = %row.id, error = %err, "skipping malformed registry row");
            None
        }
    }
}

impl RegistryDoc {
    /// The decoded row of one chat; `None` when absent or malformed.
    pub fn chat_diff_targets(&self, chat_id: &str) -> Option<ChatDiffTargetsRow> {
        self.overlay_row(KIND_CHAT_DIFF_TARGETS, chat_id)
            .and_then(|row| decode(&row))
    }

    /// Every decodable row (malformed ones are skipped), by chat id.
    pub fn all_chat_diff_targets(&self) -> Vec<ChatDiffTargetsRow> {
        let mut rows: Vec<ChatDiffTargetsRow> = self
            .overlay_rows(KIND_CHAT_DIFF_TARGETS)
            .iter()
            .filter_map(decode)
            .collect();
        rows.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        rows
    }

    /// Store `row` (id = `row.chat_id`). Compare-before-write: when the
    /// stored row already equals `row` (ignoring `updated_at`) nothing is
    /// enqueued and `false` is returned. `row.updated_at` is stored as given.
    pub fn put_chat_diff_targets(&mut self, row: &ChatDiffTargetsRow) -> Result<bool, DocError> {
        if row.chat_id.is_empty() {
            return Err(DocError::Schema("chatDiffTargets needs a chat id".into()));
        }
        if let Some(current) = self.chat_diff_targets(&row.chat_id) {
            let mut same = current.clone();
            same.updated_at = row.updated_at;
            if same == *row {
                return Ok(false);
            }
        }
        self.write(
            KIND_CHAT_DIFF_TARGETS,
            &row.chat_id,
            OpKind::Upsert,
            fields([
                ("chatId", json!(row.chat_id)),
                ("targets", json!(serde_json::to_string(&row.targets)?)),
                ("selected", opt_str(row.selected.as_deref())),
                ("ops", json!(serde_json::to_string(&row.ops)?)),
                ("updatedAt", json!(row.updated_at)),
                ("v", json!(ROW_VERSION)),
            ]),
        );
        Ok(true)
    }

    /// Delete one chat's row. `true` when it existed.
    pub fn delete_chat_diff_targets(&mut self, chat_id: &str) -> bool {
        let existed = self.row_exists(KIND_CHAT_DIFF_TARGETS, chat_id);
        if existed {
            self.delete_row_ops(&[(KIND_CHAT_DIFF_TARGETS, chat_id)]);
        }
        existed
    }

    /// Ids of the `chatDiffTargets` rows owned by any of `chat_ids` (the row
    /// id is the chat id; rows whose `chatId` field disagrees with the id
    /// are matched by either).
    pub(super) fn chat_diff_target_row_ids(&self, chat_ids: &[&str]) -> Vec<String> {
        self.overlay_rows(KIND_CHAT_DIFF_TARGETS)
            .into_iter()
            .filter(|row| {
                chat_ids.contains(&row.id.as_str())
                    || row
                        .fields
                        .get("chatId")
                        .and_then(Value::as_str)
                        .is_some_and(|id| chat_ids.contains(&id))
            })
            .map(|row| row.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::chat;
    use super::*;
    use zeron_proto::ChatDiffTargetSource;

    fn target(path: &str) -> StoredChatDiffTarget {
        StoredChatDiffTarget {
            path: path.into(),
            source: ChatDiffTargetSource::Agent,
            repo_id: "repo:abc".into(),
            worktree_id: "wt:1".into(),
            bound_at: 5,
        }
    }

    fn row(chat_id: &str) -> ChatDiffTargetsRow {
        ChatDiffTargetsRow {
            chat_id: chat_id.into(),
            targets: vec![target("/w/a"), target("/w/b")],
            selected: Some("/w/b".into()),
            ops: vec!["op-1".into()],
            updated_at: 10,
        }
    }

    #[test]
    fn chat_diff_targets_roundtrip() {
        let mut ws = RegistryDoc::new("dev-a");
        assert!(ws.chat_diff_targets("chat-1").is_none());
        assert!(ws.put_chat_diff_targets(&row("chat-1")).unwrap());
        assert_eq!(ws.chat_diff_targets("chat-1"), Some(row("chat-1")));
        assert_eq!(ws.all_chat_diff_targets(), vec![row("chat-1")]);
        let mut cleared = row("chat-1");
        cleared.selected = None;
        assert!(ws.put_chat_diff_targets(&cleared).unwrap());
        assert_eq!(ws.chat_diff_targets("chat-1").unwrap().selected, None);
        assert!(ws.delete_chat_diff_targets("chat-1"));
        assert!(!ws.delete_chat_diff_targets("chat-1"));
        assert!(ws.chat_diff_targets("chat-1").is_none());
    }

    #[test]
    fn chat_diff_targets_compare_before_write() {
        let mut ws = RegistryDoc::new("dev-a");
        assert!(ws.put_chat_diff_targets(&row("chat-1")).unwrap());
        let pending = ws.pending_len();
        assert!(!ws.put_chat_diff_targets(&row("chat-1")).unwrap());
        // A different timestamp alone is not a change.
        let mut later = row("chat-1");
        later.updated_at = 99;
        assert!(!ws.put_chat_diff_targets(&later).unwrap());
        assert_eq!(ws.pending_len(), pending);
        assert_eq!(ws.chat_diff_targets("chat-1").unwrap().updated_at, 10);
    }

    #[test]
    fn chat_diff_targets_malformed_rows_are_skipped() {
        let mut ws = RegistryDoc::new("dev-a");
        ws.put_chat_diff_targets(&row("good")).unwrap();
        ws.write(
            KIND_CHAT_DIFF_TARGETS,
            "bad-json",
            OpKind::Upsert,
            fields([
                ("chatId", json!("bad-json")),
                ("targets", json!("{not json")),
            ]),
        );
        ws.write(
            KIND_CHAT_DIFF_TARGETS,
            "no-chat",
            OpKind::Upsert,
            fields([("targets", json!("[]"))]),
        );
        let all = ws.all_chat_diff_targets();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].chat_id, "good");
        assert!(ws.chat_diff_targets("bad-json").is_none());
    }

    #[test]
    fn chat_diff_targets_delete_with_the_chat_and_space() {
        let mut ws = RegistryDoc::new("dev-a");
        ws.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
        ws.upsert_chat(&chat("chat-2", "dev-a")).unwrap();
        ws.put_chat_diff_targets(&row("chat-1")).unwrap();
        ws.put_chat_diff_targets(&row("chat-2")).unwrap();
        assert!(ws.delete_chat("chat-1").unwrap());
        assert!(ws.chat_diff_targets("chat-1").is_none());
        assert!(ws.chat_diff_targets("chat-2").is_some());

        let mut in_space = chat("chat-3", "dev-a");
        in_space.space_id = Some("sp-1".into());
        ws.upsert_chat(&in_space).unwrap();
        ws.put_chat_diff_targets(&row("chat-3")).unwrap();
        ws.delete_space("sp-1").unwrap();
        assert!(ws.chat_diff_targets("chat-3").is_none());
        assert!(ws.chat_diff_targets("chat-2").is_some());
    }
}
