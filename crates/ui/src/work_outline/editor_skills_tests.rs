// `/` skill autocomplete and auto-grow of the Work Item Details editors
// (instructions, Space-root instructions, proposal review, description).
// Included from `overview.rs`'s test module so they share its fixtures.
use super::*;

use crate::composer::AUTO_GROW_MAX_FRACTION;
use crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID;
use crate::work_outline::execution::fixture as execution_fixture;
use crate::work_outline::required_actions::fixture as m4;

/// Skill discovery (`ListCommands` params carry `harness`) answers two
/// skills and three built-in commands; every Work Outline call answers like [`reply_ok`].
fn reply_with_skills(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
    if params.get("harness").is_some() {
        return Ok(serde_json::json!([
            {"name": "code-review", "description": "Review a diff"},
            {"name": "open-pr", "description": "Open a PR"},
            {"name": "compact", "description": "Compact the conversation"},
            {"name": "clear", "description": "Clear the conversation"},
            {"name": "model", "description": "Switch the model"},
        ]));
    }
    reply_ok(params)
}

fn skill_lists(calls: &OutlineCalls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|params| params.get("harness").is_some())
        .cloned()
        .collect()
}

fn writes(calls: &OutlineCalls) -> usize {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|params| params.get("op").is_some())
        .count()
}

/// API Auth inherits API's configuration (root `/work/alpha/flagship`) and
/// carries one proposal.
fn skills_snapshot() -> WorkOutlineSnapshot {
    let mut snapshot = execution_fixture::snapshot("s1");
    snapshot.instruction_proposals.push(m4::proposal(
        "prop-auth",
        "s1",
        Some(API_AUTH),
        CHAT_AUTH,
        "Rotate keys monthly.",
    ));
    snapshot
}

/// Focus `input` in the active window, with the composer keymap bound.
fn typing_into(vcx: &mut gpui::VisualTestContext, input: &Entity<crate::composer::ComposerInput>) {
    let focus = input.read_with(vcx, |input, cx| gpui::Focusable::focus_handle(input, cx));
    vcx.update(|window, cx| {
        // The app binds the composer keymap at boot; this harness does not.
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        window.activate_window();
        window.focus(&focus, cx);
    });
    draw(vcx);
}

fn open_names(
    vcx: &mut gpui::VisualTestContext,
    skills: &Entity<crate::composer::skill_completion::SkillCompletion>,
) -> Vec<String> {
    skills.read_with(vcx, |skills, _| {
        if skills.is_open() {
            skills.visible_names()
        } else {
            Vec::new()
        }
    })
}

/// `/` in the instruction editor lists skills for the item's resolved
/// execution root; accepting inserts plain "`name` skill" text (never a
/// chip, never `/name`); Esc closes only the list (the draft survives);
/// Enter accepts instead of saving. The proposal review editor behaves the
/// same, and the Space-root editor lists the Space folder's skills.
#[gpui::test]
fn instruction_editors_insert_skills_as_plain_text(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        instruction_overview(cx, dir.path(), skills_snapshot(), reply_with_skills);
    let details = open_details(&overview, vcx, API_AUTH);
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| d.begin_instruction_draft(None, window, cx))
    });
    draw(vcx);
    let (input, skills) = details.read_with(vcx, |d, _| d.editor("instruction"));
    let skills = skills.unwrap();
    typing_into(vcx, &input);
    vcx.simulate_input("Before merging run /");
    draw(vcx);
    let lists = skill_lists(&calls);
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0]["cwd"], "/work/alpha/flagship");
    assert_eq!(lists[0]["targetDeviceId"], "d");
    // Only real skills: the built-in /compact, /clear, /model are dropped.
    assert_eq!(open_names(vcx, &skills), vec!["code-review", "open-pr"]);
    assert!(vcx.debug_bounds("skill-completion-row-0").is_some());
    assert!(vcx.debug_bounds("skill-completion-row-2").is_none());

    // Esc closes the list, not the draft.
    vcx.simulate_keystrokes("escape");
    draw(vcx);
    assert!(open_names(vcx, &skills).is_empty());
    assert!(details.read_with(vcx, |d, _| d.instruction_draft.is_some()));

    // Edit to reopen (cached: no second fetch); Enter accepts, not saves.
    vcx.simulate_input("c");
    draw(vcx);
    assert_eq!(open_names(vcx, &skills), vec!["code-review"]);
    vcx.simulate_keystrokes("enter");
    draw(vcx);
    assert_eq!(
        input.read_with(vcx, |input, _| input.text().to_owned()),
        "Before merging run `code-review` skill "
    );
    assert_eq!(skill_lists(&calls).len(), 1);
    assert_eq!(writes(&calls), 0, "Enter accepted the skill, nothing saved");
    assert!(details.read_with(vcx, |d, _| d.instruction_draft.is_some()));
    details.update(vcx, |d, cx| d.cancel_instruction(cx));
    draw(vcx);

    // Proposal review: same list, Tab accepts.
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_proposal_review("prop-auth".into(), window, cx)
        })
    });
    draw(vcx);
    let (proposal, proposal_skills) = details.read_with(vcx, |d, _| d.editor("proposal"));
    let proposal_skills = proposal_skills.unwrap();
    typing_into(vcx, &proposal);
    vcx.simulate_input(" Then /op");
    draw(vcx);
    assert_eq!(open_names(vcx, &proposal_skills), vec!["open-pr"]);
    vcx.simulate_keystrokes("tab");
    draw(vcx);
    assert_eq!(
        proposal.read_with(vcx, |input, _| input.text().to_owned()),
        "Rotate keys monthly. Then `open-pr` skill "
    );
    assert_eq!(skill_lists(&calls)[1]["cwd"], "/work/alpha/flagship");

    details.update(vcx, |d, cx| d.cancel_proposal_review(cx));
    draw(vcx);

    // Space root: the Space folder.
    let root = open_details(&overview, vcx, SPACE_ROOT_ITEM_ID);
    vcx.update(|window, cx| root.update(cx, |d, cx| d.begin_instruction_draft(None, window, cx)));
    draw(vcx);
    let (root_input, root_skills) = root.read_with(vcx, |d, _| d.editor("instruction"));
    typing_into(vcx, &root_input);
    vcx.simulate_input("/");
    draw(vcx);
    assert_eq!(
        open_names(vcx, &root_skills.unwrap()),
        vec!["code-review", "open-pr"]
    );
    assert_eq!(skill_lists(&calls).last().unwrap()["cwd"], "/work/alpha");
}

/// Skill discovery answers per folder, like Claude Code does: the Space
/// folder has its own skills (and the built-in `design` command), the `ui`
/// submodule has the Storybook `design` skill and `design-system`.
fn reply_by_folder(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
    match params.get("cwd").and_then(serde_json::Value::as_str) {
        Some("/work/alpha/ui") => Ok(serde_json::json!([
            {"name": "design", "description": "Storybook design loop"},
            {"name": "design-system", "description": "Design-system allowlist"},
            {"name": "compact", "description": "Compact the conversation"},
        ])),
        Some(_) => Ok(serde_json::json!([
            {"name": "db-query", "description": "Query the database"},
            {"name": "design", "description": "Grant or revoke Design access"},
        ])),
        None => reply_ok(params),
    }
}

/// The Design item's shape: rooted at a submodule folder below the Space
/// path, with that submodule bound as a pinned, non-primary member.
fn rooted_at_ui_submodule(snapshot: &mut WorkOutlineSnapshot, item: &str) {
    let scope = mast_proto::ExecutionScope::WorkItem {
        work_item_id: item.into(),
    };
    let mut binding = execution_fixture::binding("repo:ui", "/work/alpha/ui", true, false);
    binding.worktree_id = Some("checkout:ui".into());
    binding.worktree_path = Some("/work/alpha/ui".into());
    snapshot
        .execution_configurations
        .push(mast_proto::ExecutionConfigurationView {
            id: scope.row_id(),
            space_id: "s1".into(),
            work_item_id: Some(item.into()),
            configuration: Some(mast_proto::ExecutionConfiguration {
                execution_root: "/work/alpha/ui".into(),
                bindings: vec![binding],
                schema: 1,
            }),
            invalid_reason: None,
            updated_by: None,
            updated_at: 1,
        });
}

/// An item rooted at a subfolder (`<Space>/ui`) lists that subfolder's
/// skills in its instruction editor. The same editor first listed the Space
/// folder (no configuration yet); rooting the item there re-lists for `ui`
/// and never answers from the Space folder's cached list.
#[gpui::test]
fn instruction_editor_lists_the_subfolder_root_skills_without_leaking_the_space_list(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        instruction_overview(cx, dir.path(), skills_snapshot(), reply_by_folder);
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| d.begin_instruction_draft(None, window, cx))
    });
    draw(vcx);
    let (input, skills) = details.read_with(vcx, |d, _| d.editor("instruction"));
    let skills = skills.unwrap();
    typing_into(vcx, &input);
    vcx.simulate_input("/");
    draw(vcx);
    assert_eq!(skill_lists(&calls).last().unwrap()["cwd"], "/work/alpha");
    assert_eq!(open_names(vcx, &skills), vec!["db-query", "design"]);
    vcx.simulate_keystrokes("escape");
    draw(vcx);

    // The user roots the item at the `ui` submodule.
    let mut rooted = skills_snapshot();
    rooted_at_ui_submodule(&mut rooted, HOUSEKEEPING);
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(rooted)), cx)
    });
    draw(vcx);
    vcx.simulate_input(" /des");
    draw(vcx);
    let lists = skill_lists(&calls);
    assert_eq!(lists.len(), 2, "the new folder is fetched, not served from cache");
    assert_eq!(lists[1]["cwd"], "/work/alpha/ui");
    assert_eq!(lists[1]["targetDeviceId"], "d");
    assert_eq!(open_names(vcx, &skills), vec!["design", "design-system"]);
}

/// The instruction and description editors start at three rows, grow with
/// their text up to 40% of the window, then scroll internally with the caret
/// in view; their frames never cap (and clip) them.
#[gpui::test]
fn details_editors_grow_to_the_cap_then_scroll(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        instruction_overview(cx, dir.path(), skills_snapshot(), reply_with_skills);
    let details = open_details(&overview, vcx, API_AUTH);
    let window_height = vcx.update(|window, _| f32::from(window.viewport_size().height));
    let long = (0..200)
        .map(|n| format!("step {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    for (which, selector) in [
        ("instruction", "work-item-instruction-editor"),
        ("description", "work-item-description-editor"),
    ] {
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| match which {
                "instruction" => d.begin_instruction_draft(None, window, cx),
                _ => d.begin_description_draft(window, cx),
            })
        });
        draw(vcx);
        let (input, _) = details.read_with(vcx, |d, _| d.editor(which));
        let height = |vcx: &mut gpui::VisualTestContext| {
            f32::from(vcx.debug_bounds(selector).unwrap().size.height)
        };
        let empty = height(vcx);
        input.update(vcx, |input, cx| input.set_text("one line", cx));
        draw(vcx);
        assert_eq!(height(vcx), empty, "{which}: three rows minimum");
        input.update(vcx, |input, cx| input.set_text(vec!["x"; 8].join("\n"), cx));
        draw(vcx);
        let grown = height(vcx);
        assert!(grown > empty + 30.0, "{which} grows: {grown} vs {empty}");
        input.update(vcx, |input, cx| input.set_text(long.clone(), cx));
        draw(vcx);
        let capped = height(vcx);
        assert!(capped > grown, "{which}");
        assert!(
            capped <= window_height * AUTO_GROW_MAX_FRACTION + 12.0,
            "{which} capped: {capped} of {window_height}"
        );
        input.read_with(vcx, |input, _| {
            assert!(input.scroll_top() > 0.0, "{which} scrolled to the caret");
            assert!(input.caret_in_view(), "{which} caret visible");
        });
        if which == "instruction" {
            details.update(vcx, |d, cx| d.cancel_instruction(cx));
            draw(vcx);
        }
    }
}
