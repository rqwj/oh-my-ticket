//! Hierarchy decision functions — the pure core of create/move guarding
//! (`src/host/core.ts` concerns: `create`, `move`, `activateAncestors`).
//!
//! Guard ORDER is normative (pinned by `move-hierarchy-guards.json`):
//! self-parent is checked before child-type; child-type preempts the
//! descendant-cycle branch, which under the current HIERARCHY matrix is
//! unreachable through the public API and exists as defensive depth only.

use super::error::{Problem, Result};
use super::error::{INVALID_HIERARCHY, NOT_FOUND};
use super::store::Store;
use super::types::{hierarchy_allows, NodeRow, NodeType};

/// Require an existing node or NOT_FOUND `{kind:'node', id}`.
pub fn require_node<'a>(store: &'a Store, id: &str) -> Result<&'a NodeRow> {
    store.get_node(id).ok_or_else(|| {
        Problem::with_details(NOT_FOUND, format!("unknown node: {id}"), |d| {
            d.insert("kind".into(), "node".into());
            d.insert("id".into(), id.into());
        })
    })
}

/// Root creation rule: only epic may be created without a parent.
pub fn check_root_allowed(child_type: NodeType) -> Result<()> {
    if child_type == NodeType::Epic {
        Ok(())
    } else {
        Err(Problem::with_details(
            INVALID_HIERARCHY,
            format!("{child_type} requires a parent; only epic can be created at root"),
            |d| {
                d.insert("rule".into(), "root-requires-epic".into());
                d.insert("childType".into(), child_type.to_string().into());
            },
        ))
    }
}

/// Child-type matrix guard with `{rule:'child-type', …}` details.
pub fn check_child_type(parent: &NodeRow, child_type: NodeType) -> Result<()> {
    if hierarchy_allows(parent.node_type, child_type) {
        Ok(())
    } else {
        Err(Problem::with_details(
            INVALID_HIERARCHY,
            format!("{} cannot contain {}", parent.node_type, child_type),
            |d| {
                d.insert("rule".into(), "child-type".into());
                d.insert("parentId".into(), parent.id.clone().into());
                d.insert("parentType".into(), parent.node_type.to_string().into());
                d.insert("childType".into(), child_type.to_string().into());
            },
        ))
    }
}

/// Self-parent guard (`rule:'self-parent'`).
pub fn check_self_parent(id: &str, new_parent_id: &str) -> Result<()> {
    if id == new_parent_id {
        Err(Problem::with_details(
            INVALID_HIERARCHY,
            "a node cannot be its own parent",
            |d| {
                d.insert("rule".into(), "self-parent".into());
                d.insert("nodeId".into(), id.into());
            },
        ))
    } else {
        Ok(())
    }
}

/// Descendant-cycle guard (`rule:'descendant-cycle'`) — defensive only;
/// the child-type matrix makes it unreachable via the public API.
pub fn check_descendant_cycle(store: &Store, id: &str, new_parent_id: &str) -> Result<()> {
    for ancestor_id in ancestor_ids(store, new_parent_id) {
        if ancestor_id == id {
            return Err(Problem::with_details(
                INVALID_HIERARCHY,
                "cannot move a node under its own descendant",
                |d| {
                    d.insert("rule".into(), "descendant-cycle".into());
                    d.insert("nodeId".into(), id.into());
                    d.insert("targetParentId".into(), new_parent_id.into());
                },
            ));
        }
    }
    Ok(())
}

/// Ancestor chain of one node, closest parent first (walks until a missing
/// link or the root; a `seen` set defends against hand-made cycles).
pub fn ancestor_ids(store: &Store, id: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(id.to_string());
    let mut current = store.parent_of(id);
    while let Some(node) = current {
        if !seen.insert(node.id.clone()) {
            break;
        }
        chain.push(node.id.clone());
        current = store.parent_of(&node.id);
    }
    chain
}

/// Whole subtree below `id` (breadth-first, matching `descendantsOf`).
pub fn descendants_of(store: &Store, id: &str) -> Vec<NodeRow> {
    let mut result = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(id.to_string());
    while let Some(current) = queue.pop_front() {
        for child in store.children_of(&current) {
            result.push(child.clone());
            queue.push_back(child.id.clone());
        }
    }
    result
}
