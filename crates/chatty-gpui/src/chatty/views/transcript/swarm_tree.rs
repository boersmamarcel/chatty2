//! The swarm tree (TB-4, AGE-666): a delegation row's runs as a live tree.
//!
//! The root's turn hears every run under its delegations as the broker's
//! tagged batches, and [`SwarmTrace`] (TB-2) folds them into one tree of
//! agents. `StreamManager` keeps that trace per stream and hands it to the
//! chat view whenever it changes; the view cuts out the subtree under each
//! delegation row's callee with [`delegation_callees`], adapts it here with
//! [`adapt_swarm_tree`] and stores the result on the row, whose turn then
//! carries a [`Block::SwarmTree`](super::Block::SwarmTree).
//!
//! [`SwarmTreeCard`] draws it: one line per agent — name, model (or the
//! tool it is running), status and spend — under a header with the
//! subtree's totals. A node with children folds with its twisty. A node
//! with more than [`WIDE`] children shows its first [`SHOWN_WHEN_WIDE`] and
//! every later one that is not done, then one "+N more done" line that
//! unfolds the rest: a failure or a live run never hides, and rows keep
//! their order while statuses change.
//!
//! A line opens that agent's transcript read-only in a sheet
//! ([`AgentTranscript`]), built from what the broker forwarded: its tool
//! calls, its answer's length and its spend (a nested run's text never
//! crosses a hop). The transcript lists the agent's own sub-agents, each of
//! which opens in the same sheet, and a breadcrumb leads back up the tree.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use chatty_core::models::token_usage::{PriceBook, format_cost, format_tokens, price};
use chatty_core::services::swarm_trace::{
    NodeId, NodeStatus, SwarmTrace, ToolCall, ToolOutcome, UsageLine,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::collapsible::Collapsible as CollapsibleEl;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::GLYPH_OPACITY_MS;
use crate::assets::CustomIcon;

/// A node with more children than this shows only some of them.
pub const WIDE: usize = 8;
/// How many of a wide node's children always show, in order.
pub const SHOWN_WHEN_WIDE: usize = 6;

/// One agent of a [`SwarmTree`], as the transcript draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmNodeView {
    /// The name the broker admitted it under (`<spec>-<n>`), or its spec
    /// until the broker names it.
    pub name: String,
    pub spec: String,
    /// The model it spent most on; `None` until it reports usage.
    pub model: Option<String>,
    pub status: NodeStatus,
    /// Edges below the tree's top node, which is depth 0.
    pub depth: usize,
    /// Its parent's index in [`SwarmTree::nodes`]; `None` for the top.
    pub parent: Option<usize>,
    /// Its own spend: what it reported less what its children did.
    pub tokens: u64,
    /// That spend priced at the model roster's rates; `None` while any line
    /// of it has no price (a local model, say).
    pub cost: Option<f64>,
    pub usage: Vec<UsageLine>,
    pub turns: u32,
    /// How long its answer is; the text itself stays with its caller.
    pub text_bytes: u64,
    pub tool_calls: Vec<ToolCall>,
}

/// A delegation's subtree — its callee first, then every run under it in
/// the order a reader walks the tree — and how the reader has folded it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwarmTree {
    pub nodes: Vec<SwarmNodeView>,
    /// Nodes whose subtree the reader folded, by name.
    pub folded: BTreeSet<String>,
    /// Wide nodes whose "+N more" line the reader opened, by name.
    pub unfolded: BTreeSet<String>,
}

/// One line of the drawn tree.
#[derive(Clone, Debug, PartialEq)]
pub enum LineKind {
    /// `nodes[ix]`.
    Node(usize),
    /// The children of `parent` a wide node keeps out of view.
    More { parent: String, hidden: usize },
}

/// A line and where it sits: the guide rules in front of it and its elbow.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeLine {
    pub kind: LineKind,
    pub depth: usize,
    /// One entry per ancestor between the top and this line: whether that
    /// ancestor's rule runs past this line to a later sibling.
    pub guides: Vec<bool>,
    /// The last line under its parent: its elbow ends here.
    pub last: bool,
}

impl SwarmTree {
    pub fn running(&self) -> usize {
        self.nodes.iter().filter(|n| n.status.is_running()).count()
    }

    pub fn tokens(&self) -> u64 {
        self.nodes.iter().map(|n| n.tokens).sum()
    }

    /// The subtree's cost, when every node's spend is priced.
    pub fn cost(&self) -> Option<f64> {
        self.nodes.iter().map(|n| n.cost).sum()
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }

    pub fn node(&self, name: &str) -> Option<&SwarmNodeView> {
        self.index_of(name).map(|ix| &self.nodes[ix])
    }

    pub fn children(&self, ix: usize) -> impl Iterator<Item = usize> + '_ {
        (ix + 1..self.nodes.len()).filter(move |&c| self.nodes[c].parent == Some(ix))
    }

    /// Every run under `ix`, at any depth.
    pub fn descendants(&self, ix: usize) -> usize {
        self.nodes[ix + 1..]
            .iter()
            .take_while(|n| n.depth > self.nodes[ix].depth)
            .count()
    }

    /// The names from the top down to `name`, for a breadcrumb.
    pub fn path(&self, name: &str) -> Vec<String> {
        let mut path = Vec::new();
        let mut at = self.index_of(name);
        while let Some(ix) = at {
            path.push(self.nodes[ix].name.clone());
            at = self.nodes[ix].parent;
        }
        path.reverse();
        path
    }

    /// The same tree with `previous`'s folds, for names it still has.
    pub fn with_folds_of(mut self, previous: &SwarmTree) -> Self {
        self.folded = previous.folded.clone();
        self.unfolded = previous.unfolded.clone();
        self
    }

    /// The same tree with `name` folded if it was open, or open if folded.
    pub fn toggled(&self, name: &str) -> Self {
        let mut next = self.clone();
        if !next.folded.remove(name) {
            next.folded.insert(name.to_string());
        }
        next
    }

    /// The same tree with every child of `name` shown.
    pub fn unfolded_at(&self, name: &str) -> Self {
        let mut next = self.clone();
        next.unfolded.insert(name.to_string());
        next
    }

    /// The children of `ix` that show, and how many do not: all of them,
    /// unless it is wide and not unfolded.
    fn shown_children(&self, ix: usize) -> (Vec<usize>, usize) {
        let children: Vec<usize> = self.children(ix).collect();
        if children.len() <= WIDE || self.unfolded.contains(&self.nodes[ix].name) {
            return (children, 0);
        }
        let shown: Vec<usize> = children
            .iter()
            .enumerate()
            .filter(|(pos, c)| {
                *pos < SHOWN_WHEN_WIDE || self.nodes[**c].status != NodeStatus::Completed
            })
            .map(|(_, c)| *c)
            .collect();
        let hidden = children.len() - shown.len();
        // "+1 more" hides nothing worth a line.
        if hidden < 2 {
            return (children, 0);
        }
        (shown, hidden)
    }

    /// The lines the card draws, top down.
    pub fn lines(&self) -> Vec<TreeLine> {
        let mut lines = Vec::new();
        if !self.nodes.is_empty() {
            self.push_lines(0, Vec::new(), true, &mut lines);
        }
        lines
    }

    fn push_lines(&self, ix: usize, guides: Vec<bool>, last: bool, lines: &mut Vec<TreeLine>) {
        let depth = self.nodes[ix].depth;
        lines.push(TreeLine {
            kind: LineKind::Node(ix),
            depth,
            guides: guides.clone(),
            last,
        });
        if self.folded.contains(&self.nodes[ix].name) {
            return;
        }
        let child_guides = if depth == 0 {
            Vec::new()
        } else {
            let mut g = guides;
            g.push(!last);
            g
        };
        let (shown, hidden) = self.shown_children(ix);
        for (pos, child) in shown.iter().enumerate() {
            let last = pos + 1 == shown.len() && hidden == 0;
            self.push_lines(*child, child_guides.clone(), last, lines);
        }
        if hidden > 0 {
            lines.push(TreeLine {
                kind: LineKind::More {
                    parent: self.nodes[ix].name.clone(),
                    hidden,
                },
                depth: depth + 1,
                guides: child_guides,
                last: true,
            });
        }
    }
}

/// The root's callee for each delegation row, in the order the rows were
/// opened: the first child of the root with the row's spec that an earlier
/// row has not claimed. That is the rule [`SwarmTrace`] follows when a
/// delegation starts, so the two agree on which run is whose.
pub fn delegation_callees(trace: &SwarmTrace, specs: &[String]) -> Vec<Option<NodeId>> {
    let tree = trace.tree();
    let children = tree.children(tree.root());
    let mut claimed = vec![false; children.len()];
    specs
        .iter()
        .map(|spec| {
            let ix = (0..children.len())
                .find(|&ix| !claimed[ix] && tree.get(children[ix]).spec == *spec)?;
            claimed[ix] = true;
            Some(children[ix])
        })
        .collect()
}

/// The subtree under `top`, priced against `book`, nothing folded.
pub fn adapt_swarm_tree(trace: &SwarmTrace, top: NodeId, book: &PriceBook) -> SwarmTree {
    let tree = trace.tree();
    let mut nodes = Vec::new();
    // (node, depth, parent's index)
    let mut stack = vec![(top, 0usize, None)];
    while let Some((id, depth, parent)) = stack.pop() {
        let node = tree.get(id);
        let ix = nodes.len();
        for child in tree.children(id).iter().rev() {
            stack.push((*child, depth + 1, Some(ix)));
        }
        let cost = price(&node.token_usage(), book);
        nodes.push(SwarmNodeView {
            name: node.name.clone(),
            spec: node.spec.clone(),
            model: node.model.as_ref().map(|m| m.model_id.clone()),
            status: node.status.clone(),
            depth,
            parent,
            tokens: node.usage.iter().map(UsageLine::tokens).sum(),
            cost: (cost.unpriced_lines == 0).then_some(cost.usd),
            usage: node.usage.clone(),
            turns: node.turns,
            text_bytes: node.text_bytes,
            tool_calls: node.tool_calls.clone(),
        });
    }
    SwarmTree {
        nodes,
        ..SwarmTree::default()
    }
}

/// What a node's status is called, in the line and in its transcript.
pub fn status_label(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Running => "Running",
        NodeStatus::Completed => "Done",
        NodeStatus::Failed => "Failed",
        NodeStatus::Canceled => "Canceled",
        NodeStatus::Refused { .. } => "Refused",
    }
}

/// Tokens, and the price when there is one: `1.2K tok · $0.004`.
fn spend_label(tokens: u64, cost: Option<f64>) -> String {
    let tokens = format_tokens(u32::try_from(tokens).unwrap_or(u32::MAX));
    match cost {
        Some(usd) if usd > 0.0 => format!("{tokens} tok · {}", format_cost(usd)),
        _ => format!("{tokens} tok"),
    }
}

/// What a line says after the name: the tool a running agent is in, else
/// its model.
fn secondary_label(node: &SwarmNodeView) -> Option<String> {
    let current = node
        .status
        .is_running()
        .then(|| {
            node.tool_calls
                .iter()
                .rev()
                .find(|call| call.outcome == ToolOutcome::Running)
        })
        .flatten()
        .map(|call| format!("{}…", call.name));
    current.or_else(|| node.model.clone())
}

fn status_marker(key: &str, status: &NodeStatus, cx: &App) -> AnyElement {
    match status {
        NodeStatus::Running => div()
            .size(px(12.))
            .rounded_full()
            .border_2()
            .border_color(cx.theme().primary)
            .with_animation(
                ElementId::Name(format!("swarm-running-{key}").into()),
                Animation::new(Duration::from_millis(GLYPH_OPACITY_MS)).repeat(),
                |this, delta| {
                    let wave = (delta * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                    this.opacity(0.55 + 0.45 * wave)
                },
            )
            .into_any_element(),
        NodeStatus::Completed => Icon::new(CustomIcon::CheckCircle)
            .size_3p5()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        NodeStatus::Failed => Icon::new(CustomIcon::CircleX)
            .size_3p5()
            .text_color(cx.theme().danger)
            .into_any_element(),
        NodeStatus::Canceled => Icon::new(CustomIcon::CircleDashed)
            .size_3p5()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        NodeStatus::Refused { .. } => Icon::new(CustomIcon::TriangleAlert)
            .size_3p5()
            .text_color(cx.theme().warning)
            .into_any_element(),
    }
}

/// Width of one level of indentation, guide rule in its middle.
const INDENT: f32 = 16.;

/// The guide rules in front of a line: one column per ancestor level, with
/// a vertical rule where that ancestor's siblings continue, then the elbow
/// into this line.
fn guides(line: &TreeLine, cx: &App) -> impl IntoElement {
    // `border` all but vanishes on `group_box`; the rules have to read as
    // structure, not decoration.
    let rule = cx.theme().muted_foreground.opacity(0.35);
    let column = || div().relative().flex_shrink_0().w(px(INDENT)).h_full();
    let vertical = |height: DefiniteLength| {
        div()
            .absolute()
            .top_0()
            .left(px(INDENT / 2.))
            .w(px(1.))
            .h(height)
            .bg(rule)
    };
    div()
        .flex()
        .flex_row()
        .h_full()
        .flex_shrink_0()
        .children(
            line.guides
                .iter()
                .map(|continues| column().when(*continues, |c| c.child(vertical(relative(1.))))),
        )
        .when(line.depth > 0, |row| {
            row.child(
                column()
                    .child(vertical(if line.last {
                        relative(0.5)
                    } else {
                        relative(1.)
                    }))
                    .child(
                        div()
                            .absolute()
                            .top(relative(0.5))
                            .left(px(INDENT / 2.))
                            .w(px(INDENT / 2. - 1.))
                            .h(px(1.))
                            .bg(rule),
                    ),
            )
        })
}

/// Opens an agent's transcript, by name.
pub type OpenSwarmNode = Rc<dyn Fn(String, &mut Window, &mut App)>;
/// Folds or unfolds a node (`Toggle`) or shows all of a wide node's
/// children (`ShowAll`), by name.
pub type FoldSwarmNode = Rc<dyn Fn(String, SwarmFold, &mut App)>;
type SwarmToggle = Rc<dyn Fn(&mut App)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwarmFold {
    Toggle,
    ShowAll,
}

/// The tree under a delegation row.
#[derive(IntoElement)]
pub struct SwarmTreeCard {
    id: ElementId,
    tree: std::sync::Arc<SwarmTree>,
    open: bool,
    on_toggle: Option<SwarmToggle>,
    on_open_node: Option<OpenSwarmNode>,
    on_fold: Option<FoldSwarmNode>,
}

impl SwarmTreeCard {
    pub fn new(id: impl Into<ElementId>, tree: std::sync::Arc<SwarmTree>) -> Self {
        Self {
            id: id.into(),
            tree,
            open: true,
            on_toggle: None,
            on_open_node: None,
            on_fold: None,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn on_toggle(mut self, f: impl Fn(&mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }

    pub fn on_open_node(mut self, f: OpenSwarmNode) -> Self {
        self.on_open_node = Some(f);
        self
    }

    pub fn on_fold(mut self, f: FoldSwarmNode) -> Self {
        self.on_fold = Some(f);
        self
    }
}

/// The fold control in front of a node with children; an empty slot of the
/// same width in front of a leaf, so names line up.
fn twisty(tree: &SwarmTree, ix: usize, on_fold: Option<FoldSwarmNode>, cx: &App) -> AnyElement {
    let node = &tree.nodes[ix];
    let slot = div().flex_shrink_0().size(px(16.));
    if tree.children(ix).next().is_none() {
        return slot.into_any_element();
    }
    let folded = tree.folded.contains(&node.name);
    let name = node.name.clone();
    slot.id(ElementId::Name(format!("swarm-fold-{name}").into()))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|s| s.bg(cx.theme().muted))
        .child(
            Icon::new(if folded {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .size_3()
            .text_color(cx.theme().muted_foreground),
        )
        .when_some(on_fold, |slot, on_fold| {
            slot.on_click(move |_, _, cx| {
                // The line under it opens the transcript; this only folds.
                cx.stop_propagation();
                on_fold(name.clone(), SwarmFold::Toggle, cx)
            })
        })
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(if folded {
                "Show its agents"
            } else {
                "Hide its agents"
            })
            .build(window, cx)
        })
        .into_any_element()
}

fn node_line(
    tree: &SwarmTree,
    ix: usize,
    line: &TreeLine,
    on_open: Option<OpenSwarmNode>,
    on_fold: Option<FoldSwarmNode>,
    cx: &App,
) -> AnyElement {
    let node = &tree.nodes[ix];
    let muted = cx.theme().muted_foreground;
    let name = node.name.clone();
    let tools = node.tool_calls.len();
    // A folded node says how much it hides.
    let hidden = tree
        .folded
        .contains(&node.name)
        .then(|| tree.descendants(ix))
        .filter(|n| *n > 0);
    div()
        .id(ElementId::Name(format!("swarm-node-{name}").into()))
        .flex()
        .flex_row()
        .items_center()
        .h_7()
        .pr_2()
        .rounded_lg()
        .cursor_pointer()
        .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
        .when_some(on_open, |row, on_open| {
            let name = name.clone();
            row.on_click(move |_, window, cx| on_open(name.clone(), window, cx))
        })
        .child(guides(line, cx))
        .child(twisty(tree, ix, on_fold, cx))
        .child(
            div()
                .flex_shrink_0()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .child(status_marker(&node.name, &node.status, cx)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .gap_2()
                .ml_2()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(node.name.clone()),
                )
                .when_some(hidden, |row, hidden| {
                    row.child(div().flex_shrink_0().text_xs().text_color(muted).child(
                        match hidden {
                            1 => "1 agent".to_string(),
                            n => format!("{n} agents"),
                        },
                    ))
                })
                .when_some(secondary_label(node), |row, secondary| {
                    row.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(secondary),
                    )
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(rems(4.5))
                .text_xs()
                .text_color(match node.status {
                    NodeStatus::Failed => cx.theme().danger,
                    _ => muted,
                })
                .child(status_label(&node.status)),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(rems(4.5))
                .text_xs()
                .text_right()
                .text_color(muted)
                .child(match tools {
                    0 => String::new(),
                    1 => "1 tool".to_string(),
                    n => format!("{n} tools"),
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(rems(8.))
                .text_xs()
                .text_right()
                .text_color(muted)
                .child(if node.tokens == 0 {
                    "—".to_string()
                } else {
                    spend_label(node.tokens, node.cost)
                }),
        )
        .into_any_element()
}

fn more_line(
    parent: &str,
    hidden: usize,
    line: &TreeLine,
    on_fold: Option<FoldSwarmNode>,
    cx: &App,
) -> AnyElement {
    let parent = parent.to_string();
    div()
        .id(ElementId::Name(format!("swarm-more-{parent}").into()))
        .flex()
        .flex_row()
        .items_center()
        .h_7()
        .pr_2()
        .rounded_lg()
        .cursor_pointer()
        .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
        .when_some(on_fold, |row, on_fold| {
            let parent = parent.clone();
            row.on_click(move |_, _, cx| on_fold(parent.clone(), SwarmFold::ShowAll, cx))
        })
        .child(guides(line, cx))
        .child(div().flex_shrink_0().size(px(16.)))
        .child(
            div()
                .ml_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("+{hidden} more done · show all")),
        )
        .into_any_element()
}

impl RenderOnce for SwarmTreeCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tree = self.tree;
        let muted = cx.theme().muted_foreground;
        let agents = tree.nodes.len();
        let running = tree.running();
        let summary = match (agents, running) {
            (1, _) => "1 agent".to_string(),
            (n, 0) => format!("{n} agents"),
            (n, r) => format!("{n} agents · {r} running"),
        };
        let chevron = if self.open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let on_toggle = self.on_toggle.clone();
        let header = div()
            .id("swarm-header")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .w_full()
            .cursor_pointer()
            .on_click(move |_, _, cx| {
                if let Some(cb) = &on_toggle {
                    cb(cx);
                }
            })
            .child(Icon::new(IconName::Bot).size_3p5().text_color(muted))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child("Swarm"),
            )
            .child(div().flex_1().text_xs().text_color(muted).child(summary))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(spend_label(tree.tokens(), tree.cost())),
            )
            .child(Icon::new(chevron).size_3().text_color(muted));

        let lines: Vec<AnyElement> = tree
            .lines()
            .iter()
            .map(|line| match &line.kind {
                LineKind::Node(ix) => node_line(
                    &tree,
                    *ix,
                    line,
                    self.on_open_node.clone(),
                    self.on_fold.clone(),
                    cx,
                ),
                LineKind::More { parent, hidden } => {
                    more_line(parent, *hidden, line, self.on_fold.clone(), cx)
                }
            })
            .collect();

        div().id(self.id).w_full().child(
            CollapsibleEl::new()
                .open(self.open)
                .w_full()
                .bg(cx.theme().group_box)
                .rounded_2xl()
                .overflow_hidden()
                .child(header)
                .content(div().flex().flex_col().px_2().pb_2().children(lines)),
        )
    }
}

/// One tool call in an agent's transcript: what it called and how that
/// ended. A nested run's arguments never reach the root, so the call is
/// named by its tool alone.
fn tool_line(call: &ToolCall, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let (status, detail, detail_color) = match &call.outcome {
        ToolOutcome::Running => (NodeStatus::Running, None, muted),
        ToolOutcome::Done { result } => (NodeStatus::Completed, Some(result.clone()), muted),
        ToolOutcome::Failed { error } => {
            (NodeStatus::Failed, Some(error.clone()), cx.theme().danger)
        }
    };
    let detail = detail
        .map(|text| text.lines().next().unwrap_or_default().trim().to_string())
        .filter(|text| !text.is_empty());
    div()
        .id(ElementId::Name(format!("swarm-call-{}", call.id).into()))
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .py_1()
        .child(
            div()
                .flex_shrink_0()
                .size(px(16.))
                .mt(px(2.))
                .flex()
                .items_center()
                .justify_center()
                .child(status_marker(&call.id, &status, cx)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(call.name.clone()),
                )
                .when_some(detail, |this, detail| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(detail_color)
                            .truncate()
                            .child(detail),
                    )
                }),
        )
        .into_any_element()
}

/// How long an answer is, for a reader.
fn answer_length(bytes: u64) -> String {
    match bytes {
        0 => "No answer yet".to_string(),
        b if b < 1024 => format!("{b} bytes"),
        b => format!("{:.1} KB", b as f64 / 1024.0),
    }
}

/// One agent's transcript, read-only, for the sheet a tree line opens: a
/// breadcrumb back up the tree, what the agent is and spent, its sub-agents
/// (each opens here in turn) and its tool calls.
#[derive(IntoElement)]
pub struct AgentTranscript {
    tree: std::sync::Arc<SwarmTree>,
    ix: usize,
    on_navigate: Option<OpenSwarmNode>,
}

impl AgentTranscript {
    pub fn new(tree: std::sync::Arc<SwarmTree>, ix: usize) -> Self {
        Self {
            tree,
            ix,
            on_navigate: None,
        }
    }

    /// Where a crumb or a sub-agent leads.
    pub fn on_navigate(mut self, f: OpenSwarmNode) -> Self {
        self.on_navigate = Some(f);
        self
    }
}

impl RenderOnce for AgentTranscript {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tree = self.tree;
        let node = tree.nodes[self.ix].clone();
        let muted = cx.theme().muted_foreground;
        let navigate = self.on_navigate;

        let path = tree.path(&node.name);
        let mut crumbs = div().flex().flex_row().flex_wrap().items_center().gap_1();
        for (pos, name) in path.iter().enumerate() {
            if pos > 0 {
                crumbs = crumbs.child(Icon::new(IconName::ChevronRight).size_3().text_color(muted));
            }
            if pos + 1 == path.len() {
                crumbs = crumbs.child(
                    div()
                        .px_1()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(name.clone()),
                );
            } else {
                let target = name.clone();
                let navigate = navigate.clone();
                crumbs = crumbs.child(
                    Button::new(ElementId::Name(format!("swarm-crumb-{name}").into()))
                        .ghost()
                        .xsmall()
                        .label(name.clone())
                        .on_click(move |_, window, cx| {
                            if let Some(navigate) = &navigate {
                                navigate(target.clone(), window, cx);
                            }
                        }),
                );
            }
        }

        let fact = |label: &'static str, value: String| {
            div()
                .flex()
                .flex_row()
                .gap_3()
                .text_sm()
                .child(
                    div()
                        .w(rems(6.))
                        .flex_shrink_0()
                        .text_color(muted)
                        .child(label),
                )
                .child(div().min_w_0().flex_1().child(value))
        };
        let status = match &node.status {
            NodeStatus::Refused { reason } => format!("Refused: {reason}"),
            other => status_label(other).to_string(),
        };
        let facts = div()
            .flex()
            .flex_col()
            .gap_1()
            .pt_3()
            .child(fact("Agent", node.spec.clone()))
            .child(fact(
                "Model",
                node.model
                    .clone()
                    .unwrap_or_else(|| "Not reported yet".into()),
            ))
            .child(fact("Status", status))
            .child(fact("Turns", node.turns.to_string()))
            .child(fact("Answer", answer_length(node.text_bytes)))
            .child(fact(
                "Spend",
                if node.tokens == 0 {
                    "None reported yet".to_string()
                } else {
                    spend_label(node.tokens, node.cost)
                },
            ));

        let section = |title: String| {
            div()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(title)
        };

        let usage = node.usage.iter().map(|line| {
            let model = line
                .model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_else(|| "unknown model".into());
            let model = match &line.plugin {
                Some(plugin) => format!("{model} · {plugin}"),
                None => model,
            };
            div()
                .flex()
                .flex_row()
                .gap_3()
                .text_xs()
                .child(div().min_w_0().flex_1().truncate().child(model))
                .child(div().flex_shrink_0().text_color(muted).child(format!(
                    "{} in · {} out · {} cached",
                    format_tokens(line.input_tokens),
                    format_tokens(line.output_tokens),
                    format_tokens(line.cache_read_tokens),
                )))
        });

        let children: Vec<usize> = tree.children(self.ix).collect();
        let sub_agents = children.iter().map(|&c| {
            let child = &tree.nodes[c];
            let target = child.name.clone();
            let navigate = navigate.clone();
            div()
                .id(ElementId::Name(format!("swarm-sub-{}", child.name).into()))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h_7()
                .px_1()
                .rounded_lg()
                .cursor_pointer()
                .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                .on_click(move |_, window, cx| {
                    if let Some(navigate) = &navigate {
                        navigate(target.clone(), window, cx);
                    }
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(status_marker(
                            &format!("sub-{}", child.name),
                            &child.status,
                            cx,
                        )),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(child.name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(secondary_label(child).unwrap_or_default()),
                )
                .child(div().flex_shrink_0().text_xs().text_color(muted).child(
                    if child.tokens == 0 {
                        status_label(&child.status).to_string()
                    } else {
                        format!(
                            "{} · {}",
                            status_label(&child.status),
                            spend_label(child.tokens, child.cost)
                        )
                    },
                ))
                .child(Icon::new(IconName::ChevronRight).size_3().text_color(muted))
        });

        let calls: Vec<AnyElement> = node.tool_calls.iter().map(|c| tool_line(c, cx)).collect();
        let has_calls = !calls.is_empty();

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(crumbs)
            .child(facts)
            .when(!node.usage.is_empty(), |this| {
                this.child(section("Spend by model".into()))
                    .child(div().flex().flex_col().gap_1().children(usage))
            })
            .when(!children.is_empty(), |this| {
                this.child(section(format!("Sub-agents · {}", children.len())))
                    .child(div().flex().flex_col().children(sub_agents))
            })
            .child(section("Tool calls".into()))
            .when(has_calls, |this| {
                this.child(div().flex().flex_col().gap_1().children(calls))
            })
            .when(!has_calls, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(if node.status.is_running() {
                            "No tool calls yet."
                        } else {
                            "It made no tool calls."
                        }),
                )
            })
            .child(div().w_full().pt_4().text_xs().text_color(muted).child(
                "A nested run forwards its tool calls and spend, not its text: \
                     the answer stays with the agent that asked for it.",
            ))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would drag in `gpui::test`, which shadows the
    // built-in `#[test]` attribute.
    use super::{LineKind, SHOWN_WHEN_WIDE, SwarmTree, WIDE, adapt_swarm_tree, delegation_callees};
    use chatty_core::models::message_types::ToolSource;
    use chatty_core::models::token_usage::{ModelRef, PriceBook, TokenPricing};
    use chatty_core::services::swarm_trace::{NodeStatus, SwarmTrace};
    use chatty_core::session::SessionEvent;
    use chatty_core::settings::models::providers_store::ProviderType;
    use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
    use chatty_fabric::{CallChain, SwarmEvent, SwarmItem};
    use serde_json::json;

    fn batch(node: &str, chain: &CallChain, inner: Vec<SwarmItem>) -> SessionEvent {
        SessionEvent::SwarmEvent(SwarmEvent {
            root_task_id: chain.root_task_id.clone(),
            node: node.to_string(),
            chain: chain.clone(),
            inner,
        })
    }

    fn started(spec: &str) -> SessionEvent {
        SessionEvent::Delegation(InvokeAgentProgress::Started {
            agent_name: spec.into(),
            prompt: "go".into(),
            source: ToolSource::Local,
        })
    }

    fn usage(model: &str, input: u32, output: u32) -> SwarmItem {
        SwarmItem::Usage {
            usage: json!({ "lines": [{
                "model": { "provider": "ollama", "model_id": model },
                "inputTokens": input, "outputTokens": output,
            }] }),
        }
    }

    fn ended() -> SwarmItem {
        SwarmItem::Ended {
            state: "completed".into(),
        }
    }

    /// reviewer → { coder-0 (done, one tool), coder-1 (running) }, the
    /// reviewer still open: the shape of a live two-level swarm.
    fn scripted() -> SwarmTrace {
        let reviewer = CallChain::root("t-1").extend("reviewer").unwrap();
        let coder = reviewer.extend("coder").unwrap();
        let mut trace = SwarmTrace::new();
        for event in [
            SessionEvent::TurnStarted,
            started("reviewer"),
            batch(
                "coder-0",
                &coder,
                vec![
                    SwarmItem::TurnStarted,
                    SwarmItem::ToolCallStarted {
                        id: "c1".into(),
                        name: "write_file".into(),
                    },
                    SwarmItem::ToolCallResult {
                        id: "c1".into(),
                        result: "ok".into(),
                    },
                    SwarmItem::Text { bytes: 42 },
                    usage("coder-model", 100, 20),
                    ended(),
                ],
            ),
            batch(
                "coder-1",
                &coder,
                vec![
                    SwarmItem::TurnStarted,
                    SwarmItem::ToolCallStarted {
                        id: "c2".into(),
                        name: "read_file".into(),
                    },
                ],
            ),
        ] {
            trace.apply(&event);
        }
        trace
    }

    fn priced() -> PriceBook {
        let mut book = PriceBook::default();
        book.insert(
            ModelRef {
                provider: ProviderType::Ollama,
                model_id: "coder-model".into(),
            },
            TokenPricing {
                input_per_million: 1.0,
                output_per_million: 2.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            },
        );
        book
    }

    fn top(trace: &SwarmTrace, spec: &str) -> SwarmTree {
        let top = delegation_callees(trace, &[spec.to_string()])[0].expect("a callee");
        adapt_swarm_tree(trace, top, &PriceBook::default())
    }

    /// Each line as `(name or "+N", guides, last)`.
    fn drawn(tree: &SwarmTree) -> Vec<(String, Vec<bool>, bool)> {
        tree.lines()
            .into_iter()
            .map(|line| {
                let label = match line.kind {
                    LineKind::Node(ix) => tree.nodes[ix].name.clone(),
                    LineKind::More { hidden, .. } => format!("+{hidden}"),
                };
                (label, line.guides, line.last)
            })
            .collect()
    }

    #[test]
    fn swarm_tree_block_adapts() {
        let trace = scripted();
        let callees = delegation_callees(&trace, &["reviewer".to_string()]);
        let top = callees[0].expect("the reviewer row has a callee");
        let tree: SwarmTree = adapt_swarm_tree(&trace, top, &priced());

        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["reviewer", "coder-0", "coder-1"]);
        let shape: Vec<(usize, Option<usize>)> =
            tree.nodes.iter().map(|n| (n.depth, n.parent)).collect();
        assert_eq!(shape, [(0, None), (1, Some(0)), (1, Some(0))]);
        // The first coder's elbow continues to its sibling; the last ends.
        let lines = drawn(&tree);
        assert!(!lines[1].2);
        assert!(lines[2].2);

        let reviewer = &tree.nodes[0];
        assert_eq!(reviewer.status, NodeStatus::Running);
        assert_eq!(reviewer.model, None);

        let done = &tree.nodes[1];
        assert_eq!(done.status, NodeStatus::Completed);
        assert_eq!(done.model.as_deref(), Some("coder-model"));
        assert_eq!(done.tokens, 120);
        // 100 in at $1/M + 20 out at $2/M.
        let cost = done.cost.expect("coder-model is priced");
        assert!((cost - 0.00014).abs() < 1e-12, "{cost}");
        assert_eq!(done.tool_calls.len(), 1);
        assert_eq!(done.text_bytes, 42);

        let running = &tree.nodes[2];
        assert_eq!(running.status, NodeStatus::Running);
        assert_eq!(running.tokens, 0);
        assert_eq!(running.tool_calls[0].name, "read_file");

        assert_eq!(tree.running(), 2);
        assert_eq!(tree.tokens(), 120);
        assert_eq!(tree.path("coder-1"), ["reviewer", "coder-1"]);
    }

    #[test]
    fn two_rows_for_the_same_spec_claim_their_own_callee() {
        let mut trace = SwarmTrace::new();
        trace.apply(&started("coder"));
        trace.apply(&started("coder"));
        let specs = [
            "coder".to_string(),
            "coder".to_string(),
            "other".to_string(),
        ];
        let callees = delegation_callees(&trace, &specs);
        assert!(callees[0].is_some() && callees[1].is_some());
        assert_ne!(callees[0], callees[1]);
        assert_eq!(callees[2], None);
    }

    #[test]
    fn a_grandchild_draws_its_parents_guide_line_and_folds_away() {
        let a = CallChain::root("t-1").extend("a").unwrap();
        let b = a.extend("b").unwrap();
        let c = b.extend("c").unwrap();
        let mut trace = SwarmTrace::new();
        trace.apply(&started("a"));
        trace.apply(&batch("b-0", &b, vec![SwarmItem::TurnStarted]));
        trace.apply(&batch("c-0", &c, vec![SwarmItem::TurnStarted]));
        trace.apply(&batch("b-1", &b, vec![SwarmItem::TurnStarted]));
        let tree = top(&trace, "a");
        assert_eq!(
            drawn(&tree),
            [
                ("a".into(), vec![], true),
                ("b-0".into(), vec![], false),
                // b-0 has a later sibling, so its rule runs past c-0.
                ("c-0".into(), vec![true], true),
                ("b-1".into(), vec![], true),
            ]
        );
        assert_eq!(tree.path("c-0"), ["a", "b-0", "c-0"]);
        assert_eq!(tree.descendants(1), 1);

        let folded = tree.toggled("b-0");
        let names: Vec<String> = drawn(&folded).into_iter().map(|l| l.0).collect();
        assert_eq!(names, ["a", "b-0", "b-1"]);
        assert_eq!(folded.toggled("b-0").lines(), tree.lines());
        // A rebuilt tree keeps the reader's folds.
        assert_eq!(
            top(&trace, "a").with_folds_of(&folded).lines(),
            folded.lines()
        );
    }

    #[test]
    fn a_wide_node_keeps_its_live_and_failed_children_in_view() {
        let lead = CallChain::root("t-1").extend("lead").unwrap();
        let worker = lead.extend("w").unwrap();
        let mut trace = SwarmTrace::new();
        trace.apply(&started("lead"));
        let total = WIDE + 4;
        for n in 0..total {
            let mut inner = vec![SwarmItem::TurnStarted];
            match n {
                // One late failure, one late runner: neither may hide.
                n if n == total - 3 => inner.push(SwarmItem::Ended {
                    state: "failed".into(),
                }),
                n if n == total - 1 => {}
                _ => inner.push(ended()),
            }
            trace.apply(&batch(&format!("w-{n}"), &worker, inner));
        }
        let tree = top(&trace, "lead");
        let lines = drawn(&tree);
        let names: Vec<&str> = lines.iter().map(|l| l.0.as_str()).collect();
        let mut expected: Vec<String> = (0..SHOWN_WHEN_WIDE).map(|n| format!("w-{n}")).collect();
        expected.insert(0, "lead".into());
        expected.push(format!("w-{}", total - 3));
        expected.push(format!("w-{}", total - 1));
        let hidden = total - SHOWN_WHEN_WIDE - 2;
        expected.push(format!("+{hidden}"));
        assert_eq!(names, expected);
        // The "+N more" line is the last under its parent.
        assert!(lines.last().unwrap().2);
        assert!(!lines[lines.len() - 2].2);

        let all = drawn(&tree.unfolded_at("lead"));
        assert_eq!(all.len(), 1 + total);
    }
}
