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
//! [`SwarmTreeCard`] draws it: one line per agent — name, model, status and
//! spend — under a header with the subtree's totals. A line opens that
//! agent's transcript read-only in a sheet ([`AgentTranscript`]), built from
//! what the broker forwarded: its tool calls, its answer's length and its
//! spend. A nested run's text never crosses a hop, so the transcript says
//! how long the answer was rather than showing it.

use std::rc::Rc;
use std::time::Duration;

use chatty_core::models::token_usage::{PriceBook, format_cost, format_tokens, price};
use chatty_core::services::swarm_trace::{
    NodeId, NodeStatus, SwarmTrace, ToolCall, ToolOutcome, UsageLine,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::collapsible::Collapsible as CollapsibleEl;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use super::GLYPH_OPACITY_MS;
use crate::assets::CustomIcon;

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
    /// One entry per ancestor between the top node and this one: whether
    /// that ancestor has a later sibling, so its guide line runs past this
    /// row.
    pub guides: Vec<bool>,
    /// The last of its parent's children: its elbow ends at the row.
    pub last_child: bool,
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

/// A delegation's subtree: its callee first, then every run under it, in
/// the order a reader walks the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmTree {
    pub nodes: Vec<SwarmNodeView>,
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

    pub fn node(&self, name: &str) -> Option<&SwarmNodeView> {
        self.nodes.iter().find(|n| n.name == name)
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

/// The subtree under `top`, priced against `book`.
pub fn adapt_swarm_tree(trace: &SwarmTrace, top: NodeId, book: &PriceBook) -> SwarmTree {
    let tree = trace.tree();
    let mut nodes = Vec::new();
    // (node, depth, guides, last child)
    let mut stack = vec![(top, 0usize, Vec::<bool>::new(), true)];
    while let Some((id, depth, guides, last_child)) = stack.pop() {
        let node = tree.get(id);
        let tokens = node.usage.iter().map(UsageLine::tokens).sum();
        let cost = price(&node.token_usage(), book);
        let children = tree.children(id);
        let child_guides = if depth == 0 {
            Vec::new()
        } else {
            let mut g = guides.clone();
            g.push(!last_child);
            g
        };
        for (ix, child) in children.iter().enumerate().rev() {
            stack.push((
                *child,
                depth + 1,
                child_guides.clone(),
                ix + 1 == children.len(),
            ));
        }
        nodes.push(SwarmNodeView {
            name: node.name.clone(),
            spec: node.spec.clone(),
            model: node.model.as_ref().map(|m| m.model_id.clone()),
            status: node.status.clone(),
            depth,
            guides,
            last_child,
            tokens,
            cost: (cost.unpriced_lines == 0).then_some(cost.usd),
            usage: node.usage.clone(),
            turns: node.turns,
            text_bytes: node.text_bytes,
            tool_calls: node.tool_calls.clone(),
        });
    }
    SwarmTree { nodes }
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

/// Width of one level of indentation, guide line in its middle.
const INDENT: f32 = 16.;

/// The guide lines in front of a line: one column per ancestor level, with
/// a vertical rule where that ancestor's siblings continue, then the
/// elbow into this node.
fn guides(node: &SwarmNodeView, cx: &App) -> impl IntoElement {
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
            node.guides
                .iter()
                .map(|continues| column().when(*continues, |c| c.child(vertical(relative(1.))))),
        )
        .when(node.depth > 0, |row| {
            row.child(
                column()
                    .child(vertical(if node.last_child {
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

pub type OpenSwarmNode = Rc<dyn Fn(String, &mut Window, &mut App)>;
type SwarmToggle = Rc<dyn Fn(&mut App)>;

/// The tree under a delegation row.
#[derive(IntoElement)]
pub struct SwarmTreeCard {
    id: ElementId,
    tree: std::sync::Arc<SwarmTree>,
    open: bool,
    on_toggle: Option<SwarmToggle>,
    on_open_node: Option<OpenSwarmNode>,
}

impl SwarmTreeCard {
    pub fn new(id: impl Into<ElementId>, tree: std::sync::Arc<SwarmTree>) -> Self {
        Self {
            id: id.into(),
            tree,
            open: true,
            on_toggle: None,
            on_open_node: None,
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
}

fn node_line(node: &SwarmNodeView, on_open: Option<OpenSwarmNode>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let name = node.name.clone();
    let tools = node.tool_calls.len();
    // A running agent names what it is doing now; otherwise its model.
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
    let secondary = current.or_else(|| node.model.clone());
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
        .tooltip({
            let name = name.clone();
            move |window, cx| {
                gpui_component::tooltip::Tooltip::new(format!("Open {name}'s transcript"))
                    .build(window, cx)
            }
        })
        .child(guides(node, cx))
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
                .when_some(secondary, |row, model| {
                    row.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(model),
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

        let on_open = self.on_open_node.clone();
        let lines: Vec<AnyElement> = tree
            .nodes
            .iter()
            .map(|node| node_line(node, on_open.clone(), cx))
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
    let (marker, detail, detail_color) = match &call.outcome {
        ToolOutcome::Running => (
            status_marker(&call.id, &NodeStatus::Running, cx),
            None,
            muted,
        ),
        ToolOutcome::Done { result } => (
            status_marker(&call.id, &NodeStatus::Completed, cx),
            Some(result.clone()),
            muted,
        ),
        ToolOutcome::Failed { error } => (
            status_marker(&call.id, &NodeStatus::Failed, cx),
            Some(error.clone()),
            cx.theme().danger,
        ),
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
                .child(marker),
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

/// One agent's transcript, read-only, for the sheet a tree line opens.
#[derive(IntoElement)]
pub struct AgentTranscript {
    node: SwarmNodeView,
}

impl AgentTranscript {
    pub fn new(node: SwarmNodeView) -> Self {
        Self { node }
    }
}

impl RenderOnce for AgentTranscript {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let node = self.node;
        let muted = cx.theme().muted_foreground;
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

        let section = |title: &'static str| {
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
                .child(div().text_color(muted).child(format!(
                    "{} in · {} out · {} cached",
                    format_tokens(line.input_tokens),
                    format_tokens(line.output_tokens),
                    format_tokens(line.cache_read_tokens),
                )))
        });

        let calls: Vec<AnyElement> = node.tool_calls.iter().map(|c| tool_line(c, cx)).collect();
        let has_calls = !calls.is_empty();

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(facts)
            .when(!node.usage.is_empty(), |this| {
                this.child(section("Spend by model"))
                    .child(div().flex().flex_col().gap_1().children(usage))
            })
            .child(section("Tool calls"))
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
    use super::{SwarmTree, adapt_swarm_tree, delegation_callees};
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

    fn usage(model: &str, input: u32, output: u32) -> SwarmItem {
        SwarmItem::Usage {
            usage: json!({ "lines": [{
                "model": { "provider": "ollama", "model_id": model },
                "inputTokens": input, "outputTokens": output,
            }] }),
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
            SessionEvent::Delegation(InvokeAgentProgress::Started {
                agent_name: "reviewer".into(),
                prompt: "review it".into(),
                source: ToolSource::Local,
            }),
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
                    SwarmItem::Ended {
                        state: "completed".into(),
                    },
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

    #[test]
    fn swarm_tree_block_adapts() {
        let trace = scripted();
        let callees = delegation_callees(&trace, &["reviewer".to_string()]);
        let top = callees[0].expect("the reviewer row has a callee");
        let tree: SwarmTree = adapt_swarm_tree(&trace, top, &priced());

        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["reviewer", "coder-0", "coder-1"]);
        let depths: Vec<usize> = tree.nodes.iter().map(|n| n.depth).collect();
        assert_eq!(depths, [0, 1, 1]);
        // The first coder's elbow continues to its sibling; the last ends.
        assert!(!tree.nodes[1].last_child);
        assert!(tree.nodes[2].last_child);

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
    }

    #[test]
    fn two_rows_for_the_same_spec_claim_their_own_callee() {
        let mut trace = SwarmTrace::new();
        for _ in 0..2 {
            trace.apply(&SessionEvent::Delegation(InvokeAgentProgress::Started {
                agent_name: "coder".into(),
                prompt: "go".into(),
                source: ToolSource::Local,
            }));
        }
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
    fn a_grandchild_draws_its_parents_guide_line() {
        let a = CallChain::root("t-1").extend("a").unwrap();
        let b = a.extend("b").unwrap();
        let c = b.extend("c").unwrap();
        let mut trace = SwarmTrace::new();
        trace.apply(&SessionEvent::Delegation(InvokeAgentProgress::Started {
            agent_name: "a".into(),
            prompt: "go".into(),
            source: ToolSource::Local,
        }));
        trace.apply(&batch("b-0", &b, vec![SwarmItem::TurnStarted]));
        trace.apply(&batch("c-0", &c, vec![SwarmItem::TurnStarted]));
        trace.apply(&batch("b-1", &b, vec![SwarmItem::TurnStarted]));
        let top = delegation_callees(&trace, &["a".to_string()])[0].unwrap();
        let tree = adapt_swarm_tree(&trace, top, &PriceBook::default());
        let rows: Vec<(&str, Vec<bool>, bool)> = tree
            .nodes
            .iter()
            .map(|n| (n.name.as_str(), n.guides.clone(), n.last_child))
            .collect();
        assert_eq!(
            rows,
            [
                ("a", vec![], true),
                ("b-0", vec![], false),
                // b-0 has a later sibling, so its rule runs past c-0.
                ("c-0", vec![true], true),
                ("b-1", vec![], true),
            ]
        );
        // Unpriced spend has no cost.
        assert!(tree.nodes.iter().all(|n| n.cost.is_none() || n.tokens == 0));
    }
}
