//! Long-chat transcript: ~2000 messages with large tool outputs. First
//! paint, idle redraw and scrolling through the whole list.

use super::*;
use crate::perf::{data, measure, measure_n};
use mast_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

fn text(id: &str, t: String) -> MessagePart {
    MessagePart::Text { id: id.into(), text: t }
}

fn tool(i: usize) -> MessagePart {
    MessagePart::Tool {
        id: format!("tool{i}"),
        call: ToolCall::Exec { command: format!("cargo test -p crate-{i} -- --nocapture") },
        is_error: false,
        resolved: true,
        // Pre-strip entries carry up to 4 KB inline; newer ones a ref + size.
        output: Some(data::tool_output(4096)),
        diff: None,
        output_ref: Some(format!("chat/tool{i}")),
        output_bytes: Some(200_000),
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
    }
}

pub(crate) fn long_chat(messages: usize) -> Vec<SessionMessageEntry> {
    let body = data::markdown_plan(1800);
    (0..messages)
        .map(|i| {
            let user = i % 2 == 0;
            let parts = if user {
                vec![text("t", format!("Please look at step {i}: the build fails with the new registry ops and I do not know why."))]
            } else {
                vec![
                    MessagePart::Reasoning { id: "r".into(), text: "Thinking about the failure. ".repeat(20) },
                    tool(i),
                    tool(i + 100_000),
                    text("t", body.clone()),
                ]
            };
            SessionMessageEntry {
                id: format!("m{i}"),
                role: if user { MessageRole::User } else { MessageRole::Assistant },
                parts,
                created_at: i as i64,
                device_id: "dev".into(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            }
        })
        .collect()
}

struct Cached(Entity<Transcript>);

impl Render for Cached {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone().cached(gpui::StyleRefinement::default().size_full())
    }
}

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_transcript_long_chat(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        Cached(transcript)
    });
    let transcript = view.read_with(vcx, |v, _| v.0.clone());
    let entries = measure("transcript", "build 2000 synthetic entries", || long_chat(2000));
    let payload: usize = entries
        .iter()
        .flat_map(|e| &e.parts)
        .map(|p| match p {
            MessagePart::Text { text, .. } | MessagePart::Reasoning { text, .. } => text.len(),
            MessagePart::Tool { output, .. } => output.as_ref().map_or(0, String::len),
            _ => 0,
        })
        .sum();
    eprintln!("BENCH transcript | payload bytes={payload}");
    measure("transcript", "sync (entries -> rows)", || {
        transcript.update(vcx, |this, cx| {
            this.state.update(cx, |state, _| {
                state.selected_chat = Some("chat".into());
                state.transcript_replayed = true;
                state.transcript = entries;
                state.transcript_revision += 1;
            });
            this.sync(cx);
        })
    });
    let rows = transcript.read_with(vcx, |t, _| t.rows.len());
    eprintln!("BENCH transcript | rows={rows}");
    measure("transcript", "first draw", || draw(vcx));
    measure_n("transcript", "idle redraw", 30, |_| draw(vcx));
    measure_n("transcript", "scroll 700px + draw", 100, |_| {
        transcript.update(vcx, |t, _| t.list.scroll_by(px(700.0)));
        draw(vcx);
    });
    measure_n("transcript", "scroll back -700px + draw", 100, |_| {
        transcript.update(vcx, |t, _| t.list.scroll_by(px(-700.0)));
        draw(vcx);
    });
    // A streaming tick: the last message grows, the whole list is re-synced.
    measure_n("transcript", "stream tick (revision bump + sync + draw)", 30, |i| {
        transcript.update(vcx, |this, cx| {
            this.state.update(cx, |state, _| {
                if let Some(last) = state.transcript.last_mut() {
                    last.parts.push(text(&format!("s{i}"), format!("token {i}")));
                }
                state.transcript_revision += 1;
            });
            this.sync(cx);
        });
        draw(vcx);
    });
}
