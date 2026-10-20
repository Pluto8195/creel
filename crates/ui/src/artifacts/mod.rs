//! Planning artifacts in the UI (phase 1: design A + B).
//!
//! Contract: `docs/plans/artifacts-contract.md`. Layout:
//!
//! - [`store`] — `WatchArtifacts` snapshots per Space (fed by `AppState`'s
//!   reconciler, which follows the Work Outline watch set);
//! - [`actions`] — typed facade over the artifact RPCs;
//! - [`model`] — pure presentation logic (link resolution, header, rows,
//!   which component renders a kind);
//! - [`document`] — the sandboxed web documents (visualize wrapper) and the
//!   render-check probe;
//! - [`layout_heuristics`] — the probe's layout checks (clipped text, overlaps, …);
//! - [`tab`] — the Artifact pane tab;
//! - [`details`] — the Work Item Details "Artifacts" section;
//! - [`cards`] — transcript artifact cards;
//! - [`render_check`] — the render-check executor (offscreen web view
//!   snapshots on macOS).
#![allow(dead_code)]

pub(crate) mod actions;
pub(crate) mod cards;
pub(crate) mod compare;
pub(crate) mod document;
pub(crate) mod find;
pub(crate) mod layout_heuristics;
pub(crate) mod links;
pub(crate) mod markdown_diff;
pub(crate) mod model;
pub(crate) mod outline;
pub(crate) mod pixel_diff;
pub(crate) mod render_check;
pub(crate) mod store;
pub(crate) mod tab;

#[cfg(test)]
mod compare_tests;
#[cfg(test)]
mod find_tests;
#[cfg(test)]
pub(crate) mod tests;

pub(crate) use actions::ArtifactActions;
pub(crate) use store::{ArtifactStore, ArtifactsLoad};
