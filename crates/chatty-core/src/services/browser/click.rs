//! Agent-driven click (AGE-489): one element ref in, one left click out.
//!
//! The only thing the model can supply is a ref from the latest
//! `browser_snapshot`. No coordinates, no selectors, no JavaScript — every
//! step below is a CDP `DOM`/`Accessibility`/`Input` command against the
//! backend node that ref was issued for, and each one can refuse:
//!
//! 1. The node must still be the element the snapshot showed: same role,
//!    same name, read live via `Accessibility.getPartialAXTree`. A node that
//!    was removed does not resolve; one whose label changed since the
//!    snapshot ("Save" became "Delete") is refused rather than clicked.
//! 2. The node must be on screen after `DOM.scrollIntoViewIfNeeded` — a box
//!    with area, and a centre inside the visual viewport.
//! 3. The click point must land on the node or one of its descendants
//!    (`DOM.getNodeForLocation`). An element under a modal, a cookie banner
//!    or any other overlay is refused; the agent never clicks whatever
//!    happens to be on top of what it looked at.
//!
//! Only then does `Input.dispatchMouseEvent` fire: move, press, release.

use chromiumoxide::cdp::browser_protocol::accessibility::GetPartialAxTreeParams;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, GetContentQuadsParams, GetDocumentParams, GetNodeForLocationParams, Node, Quad,
    ScrollIntoViewIfNeededParams,
};
use chromiumoxide::cdp::browser_protocol::page::GetLayoutMetricsParams;
use chromiumoxide::page::Page;

use super::error::BrowserError;
use super::input::{self, InputModifiers, MouseAction, MouseButtonKind, MouseInput};
use super::snapshot::SnapshotNode;

/// Where a click landed, in CSS pixels of the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickPoint {
    pub x: f64,
    pub y: f64,
}

/// Click `node`, refusing unless every check in the module doc passes.
pub(super) async fn click(page: &Page, node: &SnapshotNode) -> Result<ClickPoint, BrowserError> {
    let (_, point) = locate(page, node).await?;
    press(page, point).await?;
    Ok(point)
}

/// Run every check in the module doc against `node` and return the backend
/// node plus the point a pointer event should land on. Shared with
/// [`super::typing`], which focuses a field by clicking it the same way.
pub(super) async fn locate(
    page: &Page,
    node: &SnapshotNode,
) -> Result<(BackendNodeId, ClickPoint), BrowserError> {
    let r#ref = &node.r#ref;
    let backend_id = node.backend_node_id.ok_or_else(|| {
        BrowserError::Protocol(format!(
            "ref {ref} is not backed by a DOM element and cannot be clicked; pick a nearby \
             element from the snapshot"
        ))
    })?;
    let target = BackendNodeId::new(backend_id);

    // 1. Same element as the snapshot showed.
    let live = page
        .execute(
            GetPartialAxTreeParams::builder()
                .backend_node_id(target)
                .fetch_relatives(false)
                .build(),
        )
        .await
        .map_err(|e| {
            BrowserError::Protocol(format!(
                "ref {ref} no longer exists on the page ({e}); take a new snapshot first"
            ))
        })?;
    let (role, name) = live
        .result
        .nodes
        .iter()
        .find(|n| n.backend_dom_node_id.as_ref() == Some(&target))
        .map(|n| (ax_value(n.role.as_ref()), ax_value(n.name.as_ref())))
        .unwrap_or_default();
    ensure_same_identity(node, &role, &name)?;

    // 2. On screen.
    page.execute(
        ScrollIntoViewIfNeededParams::builder()
            .backend_node_id(target)
            .build(),
    )
    .await
    .map_err(|e| BrowserError::Protocol(format!("cannot scroll ref {ref} into view: {e}")))?;
    let quads = page
        .execute(
            GetContentQuadsParams::builder()
                .backend_node_id(target)
                .build(),
        )
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot measure ref {ref}: {e}")))?;
    let point = click_point(&quads.result.quads).ok_or_else(|| {
        BrowserError::Protocol(format!(
            "ref {ref} has no visible box (hidden or zero-sized); it cannot be clicked"
        ))
    })?;
    let metrics = page
        .execute(GetLayoutMetricsParams::default())
        .await
        .map_err(|e| BrowserError::Protocol(format!("cannot read the viewport: {e}")))?;
    let viewport = &metrics.result.css_visual_viewport;
    if !within_viewport(point, viewport.client_width, viewport.client_height) {
        return Err(BrowserError::Protocol(format!(
            "ref {ref} is outside the viewport even after scrolling; it cannot be clicked"
        )));
    }

    // 3. Nothing in the way.
    let hit = page
        .execute(
            GetNodeForLocationParams::builder()
                .x(point.x as i64)
                .y(point.y as i64)
                .include_user_agent_shadow_dom(false)
                .build()
                .map_err(|e| BrowserError::Protocol(format!("invalid hit test: {e}")))?,
        )
        .await
        .map_err(|e| BrowserError::Protocol(format!("hit test for ref {ref} failed: {e}")))?;
    if hit.result.backend_node_id != target {
        let document = page
            .execute(GetDocumentParams::builder().depth(-1).pierce(true).build())
            .await
            .map_err(|e| BrowserError::Protocol(format!("cannot read the DOM: {e}")))?;
        if !subtree_contains(&document.result.root, &target, &hit.result.backend_node_id) {
            return Err(BrowserError::Protocol(format!(
                "ref {ref} is covered by another element at its centre (a dialog, banner or \
                 overlay); dismiss that first or take a new snapshot and click what is on top"
            )));
        }
    }

    Ok((target, point))
}

/// Move, press, release — what a pointer does.
pub(super) async fn press(page: &Page, point: ClickPoint) -> Result<(), BrowserError> {
    for action in [
        MouseAction::Move,
        MouseAction::Down {
            button: MouseButtonKind::Left,
            click_count: 1,
        },
        MouseAction::Up {
            button: MouseButtonKind::Left,
            click_count: 1,
        },
    ] {
        input::dispatch_mouse(
            page,
            MouseInput {
                action,
                x: point.x,
                y: point.y,
                modifiers: InputModifiers::default(),
            },
        )
        .await?;
    }
    Ok(())
}

/// The string inside a CDP `AXValue`, or empty.
pub(super) fn ax_value(
    value: Option<&chromiumoxide::cdp::browser_protocol::accessibility::AxValue>,
) -> String {
    match value.and_then(|v| v.value.as_ref()) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Refuse unless the live role and name are what the snapshot showed.
fn ensure_same_identity(
    node: &SnapshotNode,
    live_role: &str,
    live_name: &str,
) -> Result<(), BrowserError> {
    if node.role == live_role && node.name == live_name {
        return Ok(());
    }
    Err(BrowserError::Protocol(format!(
        "ref {} was {} \"{}\" when the snapshot was taken but is now {} \"{}\"; take a new \
         snapshot before clicking",
        node.r#ref, node.role, node.name, live_role, live_name
    )))
}

/// Centre of the first quad with area. Quads are eight numbers (x, y × 4),
/// clockwise, in viewport CSS pixels.
fn click_point(quads: &[Quad]) -> Option<ClickPoint> {
    quads.iter().find_map(|quad| {
        let q = quad.inner();
        if q.len() != 8 {
            return None;
        }
        let xs = [q[0], q[2], q[4], q[6]];
        let ys = [q[1], q[3], q[5], q[7]];
        let (min_x, max_x) = (
            xs.iter().cloned().fold(f64::MAX, f64::min),
            xs.iter().cloned().fold(f64::MIN, f64::max),
        );
        let (min_y, max_y) = (
            ys.iter().cloned().fold(f64::MAX, f64::min),
            ys.iter().cloned().fold(f64::MIN, f64::max),
        );
        if max_x - min_x < 1.0 || max_y - min_y < 1.0 {
            return None;
        }
        Some(ClickPoint {
            x: (min_x + max_x) / 2.0,
            y: (min_y + max_y) / 2.0,
        })
    })
}

fn within_viewport(point: ClickPoint, width: f64, height: f64) -> bool {
    point.x >= 0.0 && point.y >= 0.0 && point.x < width && point.y < height
}

/// Whether `hit` is `target` or sits inside it, walking children, shadow
/// roots and framed documents (what `DOM.getDocument` with `pierce` returns).
fn subtree_contains(root: &Node, target: &BackendNodeId, hit: &BackendNodeId) -> bool {
    fn find<'a>(node: &'a Node, id: &BackendNodeId) -> Option<&'a Node> {
        if &node.backend_node_id == id {
            return Some(node);
        }
        descendants(node).find_map(|child| find(child, id))
    }
    fn descendants(node: &Node) -> impl Iterator<Item = &Node> {
        node.children
            .iter()
            .flatten()
            .chain(node.shadow_roots.iter().flatten())
            .chain(node.content_document.iter().map(|d| d.as_ref()))
    }
    match find(root, target) {
        Some(subtree) => find(subtree, hit).is_some(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(r#ref: &str, role: &str, name: &str) -> SnapshotNode {
        SnapshotNode {
            r#ref: r#ref.to_string(),
            role: role.to_string(),
            name: name.to_string(),
            depth: 0,
            backend_node_id: Some(7),
        }
    }

    #[test]
    fn identity_must_match_the_snapshot() {
        let save = node("e2", "button", "Save");
        assert!(ensure_same_identity(&save, "button", "Save").is_ok());
        let err = ensure_same_identity(&save, "button", "Delete").unwrap_err();
        assert!(err.to_string().contains("take a new snapshot"), "{err}");
        assert!(ensure_same_identity(&save, "link", "Save").is_err());
    }

    #[test]
    fn click_point_is_the_centre_of_the_first_quad_with_area() {
        let collapsed = Quad::new(vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0]);
        let real = Quad::new(vec![10.0, 20.0, 110.0, 20.0, 110.0, 60.0, 10.0, 60.0]);
        assert_eq!(click_point(std::slice::from_ref(&collapsed)), None);
        assert_eq!(
            click_point(&[collapsed, real]),
            Some(ClickPoint { x: 60.0, y: 40.0 })
        );
        assert_eq!(click_point(&[]), None);
    }

    #[test]
    fn viewport_containment_is_half_open() {
        assert!(within_viewport(ClickPoint { x: 0.0, y: 0.0 }, 800.0, 600.0));
        assert!(!within_viewport(
            ClickPoint { x: 800.0, y: 10.0 },
            800.0,
            600.0
        ));
        assert!(!within_viewport(
            ClickPoint { x: -1.0, y: 10.0 },
            800.0,
            600.0
        ));
        assert!(!within_viewport(
            ClickPoint { x: 10.0, y: 601.0 },
            800.0,
            600.0
        ));
    }

    /// A document shaped like `DOM.getDocument(pierce)`: a button with a text
    /// child, a sibling overlay, and a shadow host with a button inside.
    fn document() -> Node {
        serde_json::from_str(
            r##"{
              "nodeId": 1, "backendNodeId": 1, "nodeType": 9, "nodeName": "#document",
              "localName": "", "nodeValue": "",
              "children": [{
                "nodeId": 2, "backendNodeId": 2, "nodeType": 1, "nodeName": "BODY",
                "localName": "body", "nodeValue": "",
                "children": [
                  {"nodeId": 3, "backendNodeId": 3, "nodeType": 1, "nodeName": "BUTTON",
                   "localName": "button", "nodeValue": "",
                   "children": [{"nodeId": 4, "backendNodeId": 4, "nodeType": 3,
                                 "nodeName": "#text", "localName": "", "nodeValue": "Save"}]},
                  {"nodeId": 5, "backendNodeId": 5, "nodeType": 1, "nodeName": "DIV",
                   "localName": "div", "nodeValue": ""},
                  {"nodeId": 6, "backendNodeId": 6, "nodeType": 1, "nodeName": "X-HOST",
                   "localName": "x-host", "nodeValue": "",
                   "shadowRoots": [{"nodeId": 7, "backendNodeId": 7, "nodeType": 11,
                                    "nodeName": "#shadow-root", "localName": "", "nodeValue": "",
                                    "children": [{"nodeId": 8, "backendNodeId": 8, "nodeType": 1,
                                                  "nodeName": "BUTTON", "localName": "button",
                                                  "nodeValue": ""}]}]}
                ]
              }]
            }"##,
        )
        .expect("fixture parses")
    }

    #[test]
    fn hit_on_the_target_or_its_text_counts_but_an_overlay_does_not() {
        let root = document();
        let id = BackendNodeId::new;
        assert!(subtree_contains(&root, &id(3), &id(3)));
        assert!(subtree_contains(&root, &id(3), &id(4)));
        assert!(!subtree_contains(&root, &id(3), &id(5)), "overlay sibling");
        assert!(
            !subtree_contains(&root, &id(4), &id(3)),
            "ancestor is not inside"
        );
        assert!(
            subtree_contains(&root, &id(6), &id(8)),
            "shadow DOM is pierced"
        );
        assert!(!subtree_contains(&root, &id(99), &id(3)), "unknown target");
    }
}
