//! Work Outline UI (Milestone 1).
//!
//! Layout and ownership (`docs/plans/work-outline/m0-ui-integration.md` §10):
//! pure, GUI-free logic lives in separately testable files; rendering files
//! depend on them, never the reverse. This scaffold commit defines only the
//! three cross-owner seams, all with stub bodies:
//!
//! 1. [`command`] — [`WorkOutlineCommand`] (the nine palette/keymap commands)
//!    and [`WorkOutlineConfirmRequest`] (the Shell-owned confirmation modal).
//! 2. [`store`] — the read side: wire view types re-exported from
//!    `mast_proto`, [`WorkOutlineLoad`], and [`WorkOutlineStore`] with a
//!    fixture constructor so presentation work needs no engine.
//! 3. [`actions`] — the mutation facade over the `WorkOutlineMutate` RPC.
//!
//! Further files land with their owners: `model.rs`, `nav.rs`, `filter.rs`,
//! `outline_view.rs`, `details_panel.rs`, `chips.rs` (presentation owner) and
//! `shell/work_outline_ui.rs` (data/shell owner).
//!
//! Any change to the types in the three seam files goes through the parent
//! orchestrator, not an individual owner.

// Scaffold: nothing consumes these yet. Remove this allow as owners wire the
// seams in.
#![allow(dead_code)]

pub(crate) mod action_log;
pub(crate) mod actions;
pub(crate) mod chips;
pub(crate) mod command;
pub(crate) mod details_panel;
pub(crate) mod execution;
pub(crate) mod filter;
pub(crate) mod hints;
pub(crate) mod instructions;
pub(crate) mod launch;
pub(crate) mod model;
pub(crate) mod nav;
pub(crate) mod optimistic;
pub(crate) mod outline_view;
pub(crate) mod required_actions;
pub(crate) mod required_actions_view;
pub(crate) mod restructure;
pub(crate) mod revision_diff;
pub(crate) mod store;
pub(crate) mod templates;
pub(crate) mod tree_guides;

#[allow(unused_imports)]
pub(crate) use actions::WorkOutlineActions;
#[allow(unused_imports)]
pub(crate) use command::{
    WorkOutlineArchiveRequest, WorkOutlineCommand, WorkOutlineConfirmRequest,
    WorkOutlineDriftRequest, WorkOutlineParentChoice,
};
#[allow(unused_imports)]
pub(crate) use store::{WorkOutlineLoad, WorkOutlineStore};
