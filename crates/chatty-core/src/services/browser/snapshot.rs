//! Accessibility-tree snapshots with stable element refs.
//!
//! The agent gets the accessibility tree, never raw HTML: roughly a tenth of the
//! tokens and better signal, because it is already the semantic view.
//!
//! Refs (`e1`, `e2`, …) are handed out with the generation they belong to.
//! Navigation bumps the generation, and so will the control-lock handback in
//! AGE-156 — in both cases the page moved and a ref from before it moved must be
//! refused rather than resolved to whatever now sits at that node.

use std::collections::HashMap;

use serde::Serialize;

use super::error::BrowserError;

/// One node the agent can refer to.
#[derive(Clone, Debug, Serialize)]
pub struct SnapshotNode {
    /// Stable handle for this node within its generation, e.g. `e12`.
    pub r#ref: String,
    pub role: String,
    pub name: String,
    /// Nesting depth, used to render the tree as indented text.
    pub depth: usize,
    /// CDP backend node id, for the interaction tools in Lane B.
    pub backend_node_id: Option<i64>,
}

/// A flattened accessibility tree plus the generation its refs belong to.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub generation: u64,
    pub nodes: Vec<SnapshotNode>,
}

impl Snapshot {
    /// Render as indented text — what actually goes into the model's context.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for node in &self.nodes {
            for _ in 0..node.depth {
                out.push_str("  ");
            }
            out.push_str(&format!("[{}] {}", node.r#ref, node.role));
            if !node.name.is_empty() {
                out.push_str(&format!(" \"{}\"", node.name));
            }
            out.push('\n');
        }
        out
    }

    /// Resolve a ref, refusing one issued before the page last moved.
    pub fn resolve(
        &self,
        r#ref: &str,
        current_generation: u64,
    ) -> Result<&SnapshotNode, BrowserError> {
        if self.generation != current_generation {
            return Err(BrowserError::StaleRef(
                r#ref.to_string(),
                self.generation,
                current_generation,
            ));
        }
        self.nodes.iter().find(|n| n.r#ref == r#ref).ok_or_else(|| {
            BrowserError::Protocol(format!("no element with ref {ref}", r#ref = r#ref))
        })
    }
}

/// Roles that carry no information for a design review and only cost tokens.
fn is_noise(role: &str, name: &str) -> bool {
    matches!(role, "none" | "generic" | "InlineTextBox" | "LineBreak") && name.is_empty()
}

/// Flatten `Accessibility.getFullAXTree` output into indented, ref-tagged nodes.
///
/// Takes the raw node list rather than a CDP type so it can be tested against a
/// captured fixture without a browser.
pub fn flatten_ax_tree(nodes: &[AxNode], generation: u64) -> Snapshot {
    let by_id: HashMap<&str, &AxNode> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();

    // The root is the node nothing else claims as a child.
    let claimed: std::collections::HashSet<&str> = nodes
        .iter()
        .flat_map(|n| n.child_ids.iter().map(|s| s.as_str()))
        .collect();
    let roots: Vec<&AxNode> = nodes
        .iter()
        .filter(|n| !claimed.contains(n.node_id.as_str()))
        .collect();

    let mut out = Vec::new();
    let mut counter = 0usize;
    for root in roots {
        walk(root, &by_id, 0, &mut counter, &mut out);
    }

    Snapshot {
        generation,
        nodes: out,
    }
}

fn walk(
    node: &AxNode,
    by_id: &HashMap<&str, &AxNode>,
    depth: usize,
    counter: &mut usize,
    out: &mut Vec<SnapshotNode>,
) {
    let role = node.role.clone().unwrap_or_default();
    let name = node.name.clone().unwrap_or_default();

    // Skip ignored and structurally empty nodes, but keep walking their children
    // at the same depth so the tree does not gain a level per wrapper div.
    let keep = !node.ignored && !is_noise(&role, &name);
    let child_depth = if keep {
        *counter += 1;
        out.push(SnapshotNode {
            r#ref: format!("e{counter}"),
            role,
            name,
            depth,
            backend_node_id: node.backend_dom_node_id,
        });
        depth + 1
    } else {
        depth
    };

    for child_id in &node.child_ids {
        if let Some(child) = by_id.get(child_id.as_str()) {
            walk(child, by_id, child_depth, counter, out);
        }
    }
}

/// The subset of `Accessibility.AXNode` we read.
///
/// Deserialized straight from the CDP JSON so the flattening logic can be tested
/// against a captured tree with no browser in the loop.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNode {
    pub node_id: String,
    #[serde(default)]
    pub ignored: bool,
    #[serde(default, deserialize_with = "value_of")]
    pub role: Option<String>,
    #[serde(default, deserialize_with = "value_of")]
    pub name: Option<String>,
    #[serde(default)]
    pub child_ids: Vec<String>,
    /// CDP spells this `backendDOMNodeId`, which is not what `camelCase`
    /// derives from the field name — hence the explicit rename.
    #[serde(default, rename = "backendDOMNodeId")]
    pub backend_dom_node_id: Option<i64>,
}

/// CDP wraps role and name as `{ "type": ..., "value": ... }`.
fn value_of<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|v| match v.get("value") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
        None => None,
    }))
}

use serde::Deserialize as _;

#[cfg(test)]
mod tests {
    use super::*;

    /// A small tree shaped like real CDP output: a root, a wrapper the tree
    /// should skip, and two interesting leaves.
    fn fixture() -> Vec<AxNode> {
        serde_json::from_str(
            r#"[
              {"nodeId":"1","ignored":false,
               "role":{"type":"role","value":"RootWebArea"},
               "name":{"type":"computedString","value":"Dashboard"},
               "childIds":["2"],"backendDOMNodeId":1},
              {"nodeId":"2","ignored":false,
               "role":{"type":"role","value":"generic"},
               "name":{"type":"computedString","value":""},
               "childIds":["3","4"],"backendDOMNodeId":2},
              {"nodeId":"3","ignored":false,
               "role":{"type":"role","value":"button"},
               "name":{"type":"computedString","value":"Save"},
               "childIds":[],"backendDOMNodeId":3},
              {"nodeId":"4","ignored":true,
               "role":{"type":"role","value":"StaticText"},
               "name":{"type":"computedString","value":"hidden"},
               "childIds":["5"],"backendDOMNodeId":4},
              {"nodeId":"5","ignored":false,
               "role":{"type":"role","value":"heading"},
               "name":{"type":"computedString","value":"Totals"},
               "childIds":[],"backendDOMNodeId":5}
            ]"#,
        )
        .expect("fixture parses")
    }

    #[test]
    fn parses_cdp_role_and_name_wrappers() {
        let nodes = fixture();
        assert_eq!(nodes[0].role.as_deref(), Some("RootWebArea"));
        assert_eq!(nodes[0].name.as_deref(), Some("Dashboard"));
    }

    #[test]
    fn flattens_from_the_root_and_assigns_sequential_refs() {
        let snapshot = flatten_ax_tree(&fixture(), 7);
        let refs: Vec<&str> = snapshot.nodes.iter().map(|n| n.r#ref.as_str()).collect();
        assert_eq!(refs, vec!["e1", "e2", "e3"]);
        assert_eq!(snapshot.generation, 7);
    }

    #[test]
    fn skips_wrapper_and_ignored_nodes_but_keeps_their_children() {
        let snapshot = flatten_ax_tree(&fixture(), 1);
        let roles: Vec<&str> = snapshot.nodes.iter().map(|n| n.role.as_str()).collect();
        assert_eq!(roles, vec!["RootWebArea", "button", "heading"]);

        // The empty `generic` wrapper is skipped, so `button` sits directly
        // under the root rather than gaining a level.
        let button = &snapshot.nodes[1];
        assert_eq!(button.name, "Save");
        assert_eq!(button.depth, 1);

        // `heading` survives its ignored parent, at that parent's depth.
        let heading = &snapshot.nodes[2];
        assert_eq!(heading.name, "Totals");
        assert_eq!(heading.depth, 1);
    }

    #[test]
    fn renders_indented_text() {
        let text = flatten_ax_tree(&fixture(), 1).to_text();
        assert_eq!(
            text,
            "[e1] RootWebArea \"Dashboard\"\n  [e2] button \"Save\"\n  [e3] heading \"Totals\"\n"
        );
    }

    #[test]
    fn resolves_a_ref_within_its_generation() {
        let snapshot = flatten_ax_tree(&fixture(), 3);
        let node = snapshot.resolve("e2", 3).expect("ref resolves");
        assert_eq!(node.name, "Save");
        assert_eq!(node.backend_node_id, Some(3));
    }

    #[test]
    fn refuses_a_ref_from_an_older_generation() {
        let snapshot = flatten_ax_tree(&fixture(), 3);
        let err = snapshot.resolve("e2", 4).unwrap_err();
        assert!(
            matches!(err, BrowserError::StaleRef(ref r, 3, 4) if r == "e2"),
            "expected a stale-ref error, got {err}"
        );
    }

    #[test]
    fn unknown_refs_are_an_error_not_a_silent_miss() {
        let snapshot = flatten_ax_tree(&fixture(), 1);
        assert!(snapshot.resolve("e99", 1).is_err());
    }

    /// The real path serializes chromiumoxide's `AxNode` and deserializes into
    /// ours. A field rename on either side would silently drop data, so pin the
    /// contract here rather than discovering it against a live browser.
    #[test]
    fn round_trips_from_the_chromiumoxide_ax_node_type() {
        use chromiumoxide::cdp::browser_protocol::accessibility as ax;

        let source = ax::AxNode {
            node_id: ax::AxNodeId::new("42"),
            ignored: false,
            ignored_reasons: None,
            role: Some(ax::AxValue::new(ax::AxValueType::Role)),
            chrome_role: None,
            name: None,
            description: None,
            value: None,
            properties: None,
            parent_id: None,
            child_ids: Some(vec![ax::AxNodeId::new("43")]),
            backend_dom_node_id: Some(
                chromiumoxide::cdp::browser_protocol::dom::BackendNodeId::new(99),
            ),
            frame_id: None,
        };

        let json = serde_json::to_value(vec![&source]).expect("serializes");
        let ours: Vec<AxNode> = serde_json::from_value(json).expect("deserializes into ours");

        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].node_id, "42");
        assert_eq!(ours[0].child_ids, vec!["43".to_string()]);
        assert_eq!(
            ours[0].backend_dom_node_id,
            Some(99),
            "backendDOMNodeId must survive the round trip"
        );
    }

    #[test]
    fn empty_tree_is_not_a_panic() {
        let snapshot = flatten_ax_tree(&[], 1);
        assert!(snapshot.nodes.is_empty());
        assert_eq!(snapshot.to_text(), "");
    }
}
