//! Composable multi-dimension grouping for the multichat overview list —
//! a port of `session_canvas.html`'s `GROUP_DIMENSIONS`/`activeGroups`/
//! `partitionAndPlace` (verified by reading that file directly with
//! `grep -a`/a non-grep tool; plain `grep` silently treats the whole file as
//! binary and skips real matches because of a stray null byte in it — a
//! previous pass got a false "this function doesn't exist" negative exactly
//! this way).
//!
//! The dimensions toggle independently and *compose*: the
//! order you activate them in is the nesting order (a `Set`'s insertion
//! order in JS — an ordered `Vec` here is the direct equivalent and doesn't
//! need any of `Set`'s other semantics). `partitionAndPlace`
//! (session_canvas.html:1499-1541) recurses over the active dimensions;
//! once dimensions run out the remaining rows are the leaves (see
//! [`Partition::Leaves`]). The list renders each level as a header with its
//! rows nested beneath.
//!
//! This module is the pure partitioning step only: given rows and an
//! ordered list of active dimensions, produce the nested group tree. It
//! knows nothing about pixel layout, colors, or display labels (the
//! reference's `getMeta`) — those are UI-layer concerns a caller adds on
//! top of the raw group keys this module returns, via [`GroupKeyed`].

use std::collections::HashMap;

/// One of the composable grouping dimensions
/// (`session_canvas.html:1411-1420`'s `GROUP_DIMENSIONS`). Nesting order is
/// caller-determined via the order dimensions appear in the `dims` slice
/// passed to [`partition`] — there is no fixed priority between dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupDimension {
    /// The transcript's cwd, last path segment.
    ///
    /// Real and already in use: `session_canvas_server.py`'s own
    /// `repoGroupKey` (`session_canvas.html:909-913`) uses exactly this
    /// proxy rather than a resolved repo name, specifically to avoid a
    /// git/gh round trip per session just for a label. Already the basis of
    /// `overview.rs`'s existing single-dimension "By repo" grouping.
    Repo,
    /// A linked ticket identifier, or `"PR #<number>"` when there's a
    /// linked PR but no ticket (`ticketGroupKey`,
    /// `session_canvas.html:896-898`).
    ///
    /// Already the basis of `overview.rs`'s existing "By ticket" grouping,
    /// via `TicketStatus.identifier`.
    Ticket,
}

impl GroupDimension {
    /// Stable persistence key (`overview-ui-flags.json`'s `groupBy:` entry).
    /// Never renamed: a persisted value must keep restoring.
    pub fn persist_key(self) -> &'static str {
        match self {
            GroupDimension::Repo => "repo",
            GroupDimension::Ticket => "ticket",
        }
    }

    /// Inverse of [`GroupDimension::persist_key`]; `None` for an unknown key
    /// (a dimension from a newer/older build, such as the removed `category`
    /// and `origin`, or junk).
    pub fn from_persist_key(key: &str) -> Option<Self> {
        match key {
            "repo" => Some(GroupDimension::Repo),
            "ticket" => Some(GroupDimension::Ticket),
            _ => None,
        }
    }
}

/// Sentinel key for "this row has no value on this dimension" — matches the
/// reference's own literal `"__none__"` string (`repoGroupKey`/
/// `ticketGroupKey`, `session_canvas.html:898,912`) rather than a Rust
/// `Option`, so a dynamic-order dimension's "no value" bucket sorts and
/// buckets exactly like every other key with no special-casing needed here.
pub const NONE_KEY: &str = "__none__";

/// A row's raw group key per dimension — the only thing this module needs
/// from a caller's row type, so it stays generic over whatever `overview.rs`
/// (or any future caller) actually uses without depending on that crate's
/// types directly. Mirrors `GROUP_DIMENSIONS[dim].getKey`
/// (`session_canvas.html:1412-1419`).
pub trait GroupKeyed {
    /// The raw key for `dim`. Use [`NONE_KEY`] for "no value" (matching the
    /// reference's own convention), not an empty string or a sentinel of
    /// your own — [`compute_order`] keys off this exact constant.
    fn group_key(&self, dim: GroupDimension) -> String;
}

/// The ordered set of group keys `rows` should be bucketed into for `dim` —
/// mirrors `computeGroupOrder` (`session_canvas.html:1429-1437`). The order
/// is computed fresh from whatever keys `rows` actually has, alphabetically,
/// with [`NONE_KEY`] always sorted last (matching the reference's explicit
/// `if (a === "__none__") return 1;` / `if (b === "__none__") return -1;`).
fn compute_order<T: GroupKeyed>(rows: &[T], dim: GroupDimension) -> Vec<String> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut keys = Vec::new();
    for row in rows {
        let key = row.group_key(dim);
        if seen.insert(key.clone(), ()).is_none() {
            keys.push(key);
        }
    }
    keys.sort_by(
        |a, b| match (a.as_str() == NONE_KEY, b.as_str() == NONE_KEY) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => a.cmp(b),
        },
    );
    keys
}

/// Buckets `rows` by `dim`, dropping empty buckets, preserving `dim`'s
/// order for the ones that survive — mirrors `groupSessions`
/// (`session_canvas.html:1439-1448`). The order is derived from the rows
/// themselves, so every row's raw key is in it by construction.
fn group_rows<T: GroupKeyed>(rows: Vec<T>, dim: GroupDimension) -> Vec<(String, Vec<T>)> {
    let order = compute_order(&rows, dim);
    let index_of: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let mut buckets: Vec<Vec<T>> = (0..order.len()).map(|_| Vec::new()).collect();
    for row in rows {
        let idx = index_of[row.group_key(dim).as_str()];
        buckets[idx].push(row);
    }
    order
        .into_iter()
        .zip(buckets)
        .filter(|(_, items)| !items.is_empty())
        .collect()
}

/// One group at some depth in a [`Partition`] tree — a raw key (map it to a
/// display label/color yourself, e.g. a hash-based color matching the
/// reference's `hashColor`, `session_canvas.html:884-889`) plus its
/// recursively-partitioned contents.
#[derive(Debug, Clone, PartialEq)]
pub struct PartitionGroup<T> {
    pub key: String,
    pub items: Partition<T>,
}

/// The result of recursively partitioning rows by an ordered list of
/// dimensions. Mirrors `partitionAndPlace`'s recursion structure
/// (`session_canvas.html:1499-1541`) minus label/color metadata (a UI-layer
/// concern — see [`PartitionGroup`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Partition<T> {
    /// No active dimensions left (or none were active to begin with): the
    /// rows to render directly. A caller wanting flat/ungrouped output
    /// should call [`partition`] with an empty `dims` slice and get back
    /// exactly one `Leaves` containing every row, in input order.
    Leaves(Vec<T>),
    /// One active dimension at this depth: its rows split into ordered
    /// groups (empty ones dropped), each recursively partitioned by
    /// whatever dimensions remain. How the levels are drawn is a rendering
    /// decision for whatever consumes this tree, not encoded here (the
    /// depth is implicit in tree nesting, so a caller can track it by
    /// recursion depth while walking this).
    Groups {
        dimension: GroupDimension,
        groups: Vec<PartitionGroup<T>>,
    },
}

/// Recursively partitions `rows` by `dims`, in the given order — the
/// nesting order IS the order dimensions appear in this slice, matching the
/// reference's click-order-via-insertion-order `activeGroups` semantics
/// exactly (session_canvas.html:1421-1428's comment explains why a `Set`
/// was chosen there for this reason; an ordered `Vec` here is the direct,
/// simpler equivalent — Rust doesn't need insertion-ordered-set semantics
/// smuggled through a `HashSet`/`BTreeSet` to get the same effect).
///
/// An empty `dims` (no active dimensions) returns `Partition::Leaves(rows)`
/// unchanged — the flat/ungrouped case, and also what today's
/// single-dimension "Flat" option in `overview.rs` already means.
pub fn partition<T: GroupKeyed>(rows: Vec<T>, dims: &[GroupDimension]) -> Partition<T> {
    match dims.split_first() {
        None => Partition::Leaves(rows),
        Some((&dimension, rest)) => {
            let groups = group_rows(rows, dimension)
                .into_iter()
                .map(|(key, items)| PartitionGroup {
                    key,
                    items: partition(items, rest),
                })
                .collect();
            Partition::Groups { dimension, groups }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row {
        id: &'static str,
        repo: &'static str,
        ticket: &'static str,
    }

    impl GroupKeyed for Row {
        fn group_key(&self, dim: GroupDimension) -> String {
            match dim {
                GroupDimension::Repo => self.repo.to_string(),
                GroupDimension::Ticket => self.ticket.to_string(),
            }
        }
    }

    fn sample_rows() -> Vec<Row> {
        vec![
            Row {
                id: "a",
                repo: "zeron",
                ticket: "ENG-1",
            },
            Row {
                id: "b",
                repo: "zeron",
                ticket: NONE_KEY,
            },
            Row {
                id: "c",
                repo: "workspace",
                ticket: "ENG-1",
            },
            Row {
                id: "d",
                repo: "workspace",
                ticket: NONE_KEY,
            },
            Row {
                id: "e",
                repo: NONE_KEY,
                ticket: "ENG-2",
            },
        ]
    }

    fn ids_in_order(partition: &Partition<Row>) -> Vec<&'static str> {
        match partition {
            Partition::Leaves(rows) => rows.iter().map(|r| r.id).collect(),
            Partition::Groups { groups, .. } => {
                groups.iter().flat_map(|g| ids_in_order(&g.items)).collect()
            }
        }
    }

    #[test]
    fn zero_dimensions_is_flat_leaves_in_input_order() {
        let rows = sample_rows();
        let expected: Vec<&str> = rows.iter().map(|r| r.id).collect();
        let result = partition(rows, &[]);
        assert_eq!(result, Partition::Leaves(sample_rows()));
        assert_eq!(ids_in_order(&result), expected);
    }

    #[test]
    fn single_dimension_groups_by_repo_matching_todays_single_select_behavior() {
        let result = partition(sample_rows(), &[GroupDimension::Repo]);
        let Partition::Groups { dimension, groups } = &result else {
            panic!("expected Groups");
        };
        assert_eq!(*dimension, GroupDimension::Repo);
        // Dynamic order: alphabetical, NONE_KEY last.
        let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, vec!["workspace", "zeron", NONE_KEY]);
        let workspace = &groups[0];
        assert_eq!(ids_in_order(&workspace.items), vec!["c", "d"]);
        let zeron = &groups[1];
        assert_eq!(ids_in_order(&zeron.items), vec!["a", "b"]);
        let none = &groups[2];
        assert_eq!(ids_in_order(&none.items), vec!["e"]);
    }

    #[test]
    fn two_dimensions_compose_and_nesting_order_follows_activation_order() {
        // Repo outer, Ticket inner.
        let repo_then_ticket = partition(
            sample_rows(),
            &[GroupDimension::Repo, GroupDimension::Ticket],
        );
        let Partition::Groups { dimension, groups } = &repo_then_ticket else {
            panic!("expected Groups");
        };
        assert_eq!(*dimension, GroupDimension::Repo);
        let mast_group = groups.iter().find(|g| g.key == "zeron").unwrap();
        let Partition::Groups {
            dimension: inner_dim,
            groups: inner_groups,
        } = &mast_group.items
        else {
            panic!("expected nested Groups");
        };
        assert_eq!(*inner_dim, GroupDimension::Ticket);
        let inner_keys: Vec<&str> = inner_groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(inner_keys, vec!["ENG-1", NONE_KEY]);

        // Ticket outer, Repo inner — same rows, opposite activation order,
        // must produce a genuinely different top-level split. This is the
        // actual "composability" behavior, verified explicitly rather than
        // assumed correct from the recursion alone.
        let ticket_then_repo = partition(
            sample_rows(),
            &[GroupDimension::Ticket, GroupDimension::Repo],
        );
        let Partition::Groups {
            dimension: outer_dim2,
            groups: outer_groups2,
        } = &ticket_then_repo
        else {
            panic!("expected Groups");
        };
        assert_eq!(*outer_dim2, GroupDimension::Ticket);
        let eng1_group = outer_groups2.iter().find(|g| g.key == "ENG-1").unwrap();
        let Partition::Groups {
            dimension: inner_dim2,
            groups: inner_groups2,
        } = &eng1_group.items
        else {
            panic!("expected nested Groups");
        };
        assert_eq!(*inner_dim2, GroupDimension::Repo);
        let inner_keys2: Vec<&str> = inner_groups2.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(inner_keys2, vec!["workspace", "zeron"]);

        // Same rows end up in the tree either way, just nested differently —
        // the two orderings are not the same tree shape.
        assert_ne!(repo_then_ticket, ticket_then_repo);
        let mut ids_a = ids_in_order(&repo_then_ticket);
        let mut ids_b = ids_in_order(&ticket_then_repo);
        ids_a.sort_unstable();
        ids_b.sort_unstable();
        assert_eq!(
            ids_a, ids_b,
            "same underlying rows either way, only the nesting differs"
        );
    }

    #[test]
    fn empty_rows_produce_no_groups_for_a_dynamic_dimension() {
        let result = partition(Vec::<Row>::new(), &[GroupDimension::Repo]);
        assert_eq!(
            result,
            Partition::Groups {
                dimension: GroupDimension::Repo,
                groups: vec![]
            }
        );
    }

    #[test]
    fn persist_keys_round_trip_and_removed_dimensions_do_not_load() {
        for dim in [GroupDimension::Repo, GroupDimension::Ticket] {
            assert_eq!(
                GroupDimension::from_persist_key(dim.persist_key()),
                Some(dim)
            );
        }
        assert_eq!(GroupDimension::from_persist_key("category"), None);
        assert_eq!(GroupDimension::from_persist_key("origin"), None);
    }
}
