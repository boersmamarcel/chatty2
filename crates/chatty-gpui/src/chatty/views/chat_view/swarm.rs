//! The chat view's half of the swarm tree (TB-4, AGE-666).
//!
//! `StreamManager` folds every event of a turn into a [`SwarmTrace`] and
//! hands it here each time the swarm changes. The view remembers which
//! delegation rows the running stream opened, cuts the subtree under each
//! row's callee out of the trace and stores it on the row's
//! `DisplayMessage`, where the adapter turns it into a `Block::SwarmTree`.
//! A row whose callee delegated to no one keeps the plain delegation row it
//! always had. The reader's folds live on the stored tree and carry over
//! to each rebuilt one.
//!
//! A tree line opens that agent's transcript in a sheet. The sheet reads the
//! row's current tree each time it draws, so a transcript opened on a
//! running agent keeps up with it; its breadcrumb and sub-agent list move
//! the sheet up and down the tree, and Escape or its close button leave.
//!
//! A running agent's line and its sheet stop that agent and its subtree
//! through the desktop's broker (TB-7, AGE-749); the broker's report of the
//! stop is what turns its line to Canceled.
//!
//! A running agent's sheet also has a message box (TM-5, AGE-750): what the
//! human types there reaches that agent at its next tool call, wrapped as
//! untrusted data, through the same broker.

use std::sync::Arc;

use chatty_core::models::token_usage::PriceBook;
use chatty_core::services::swarm_trace::{NodeStatus, SwarmTrace};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme, WindowExt as _, v_flex};

use super::ChatView;
use crate::chatty::views::transcript::{
    AgentTranscript, SwarmFold, SwarmTree, adapt_swarm_tree, delegation_callees,
};

/// Stop `name` — a node of the running swarm, or the spec of the root's
/// own callee — and everything under it, through the desktop's broker
/// (TB-7). The rest of the swarm, and the turn, keep running.
pub(crate) fn stop_swarm_node(name: &str, cx: &mut App) {
    let broker = cx
        .try_global::<crate::settings::models::DiscoveredModulesModel>()
        .and_then(|modules| modules.lazy_broker.clone());
    let Some(broker) = broker else {
        tracing::warn!(agent = %name, "Stop: no broker is running");
        return;
    };
    match broker.cancel(name) {
        Ok(()) => tracing::info!(agent = %name, "Stopped an agent of the swarm"),
        Err(error) => tracing::warn!(agent = %name, %error, "Could not stop an agent"),
    }
}

/// The desktop's broker, if one is configured.
fn swarm_broker(cx: &App) -> Option<Arc<dyn chatty_core::services::lazy_broker::LazyBroker>> {
    cx.try_global::<crate::settings::models::DiscoveredModulesModel>()
        .and_then(|modules| modules.lazy_broker.clone())
}

/// Wide enough for a tool row's headline and its result preview.
const TRANSCRIPT_SHEET_WIDTH: f32 = 520.;

impl ChatView {
    /// The delegation row just opened by `start_delegation_progress`
    /// delegated to `spec`.
    pub fn note_swarm_row(&mut self, spec: &str) {
        if let Some(idx) = self.delegation_progress_msg_idx {
            self.swarm_rows.push((idx, spec.to_string()));
        }
    }

    /// A new stream has its own trace, so its rows start over.
    pub fn reset_swarm_rows(&mut self) {
        self.swarm_rows.clear();
    }

    /// Hang each delegation row's subtree of `trace` under it, priced
    /// against `book`. Redraws only when a row's tree changed.
    pub fn set_swarm_trace(
        &mut self,
        trace: &SwarmTrace,
        book: &PriceBook,
        cx: &mut Context<Self>,
    ) {
        let specs: Vec<String> = self.swarm_rows.iter().map(|(_, s)| s.clone()).collect();
        let callees = delegation_callees(trace, &specs);
        let mut changed = false;
        for ((idx, _), callee) in self.swarm_rows.iter().zip(callees) {
            let Some(top) = callee else {
                continue;
            };
            if trace.tree().children(top).is_empty() {
                // No sub-agents: the row stays plain, but ↗ on it can still
                // open this one agent's run (AGE-813).
                let mut run = adapt_swarm_tree(trace, top, book);
                if let Some(previous) = self.delegation_runs.get(idx) {
                    run = run.with_folds_of(previous);
                }
                if self.delegation_runs.get(idx).map(|r| &**r) != Some(&run) {
                    self.delegation_runs.insert(*idx, Arc::new(run));
                    changed = true;
                }
                continue;
            }
            let Some(msg) = self.messages.get_mut(*idx) else {
                continue;
            };
            let mut tree = adapt_swarm_tree(trace, top, book);
            if let Some(previous) = &msg.swarm_tree {
                tree = tree.with_folds_of(previous);
            }
            if msg.swarm_tree.as_deref() != Some(&tree) {
                msg.swarm_tree = Some(Arc::new(tree));
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// The stream ended: nothing more arrives for its swarm, so a run still
    /// drawn as running was stopped with the turn.
    pub fn settle_swarm_trees(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let settle = |tree: &Arc<SwarmTree>| {
            let mut settled = SwarmTree::clone(tree);
            for node in &mut settled.nodes {
                if node.status.is_running() {
                    node.status = NodeStatus::Canceled;
                }
            }
            Arc::new(settled)
        };
        for (idx, _) in &self.swarm_rows {
            if let Some(msg) = self.messages.get_mut(*idx)
                && let Some(tree) = msg.swarm_tree.as_ref().filter(|t| t.running() > 0)
            {
                msg.swarm_tree = Some(settle(tree));
                changed = true;
            }
            if let Some(run) = self.delegation_runs.get_mut(idx)
                && run.running() > 0
            {
                *run = settle(run);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Fold or unfold `name` in the tree under message `msg_idx`, or show
    /// all of its children.
    pub fn fold_swarm_node(
        &mut self,
        msg_idx: usize,
        name: &str,
        fold: SwarmFold,
        cx: &mut Context<Self>,
    ) {
        let Some(msg) = self.messages.get_mut(msg_idx) else {
            return;
        };
        let Some(tree) = msg.swarm_tree.as_ref() else {
            return;
        };
        let next = match fold {
            SwarmFold::Toggle => tree.toggled(name),
            SwarmFold::ShowAll => tree.unfolded_at(name),
        };
        msg.swarm_tree = Some(Arc::new(next));
        cx.notify();
    }

    /// The run under message `msg_idx`: its swarm tree, or the one-node tree
    /// of a worker that delegated to no one.
    pub(super) fn run_tree(&self, msg_idx: usize) -> Option<Arc<SwarmTree>> {
        self.messages
            .get(msg_idx)
            .and_then(|msg| msg.swarm_tree.clone())
            .or_else(|| self.delegation_runs.get(&msg_idx).cloned())
    }

    /// The top agent of the run under message `msg_idx`: what ↗ on its
    /// delegation row opens. `None` before the broker has reported a worker.
    pub(super) fn delegation_run_name(&self, msg_idx: usize) -> Option<String> {
        self.run_tree(msg_idx)
            .and_then(|tree| tree.nodes.first().map(|node| node.name.clone()))
    }

    /// Open `name`'s transcript, from the tree under message `msg_idx`.
    pub fn open_swarm_node(
        &mut self,
        msg_idx: usize,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat_view = cx.entity();
        let transcript =
            cx.new(|cx| SwarmNodeTranscript::new(chat_view, msg_idx, name, window, cx));
        window.open_sheet(cx, move |sheet, _window, _cx| {
            sheet
                .title("Agent transcript")
                .size(px(TRANSCRIPT_SHEET_WIDTH))
                .child(transcript.clone())
        });
    }
}

/// The sheet's body: one agent's transcript, redrawn whenever the chat view
/// changes so a running agent's calls keep arriving. Which agent it shows
/// moves with the breadcrumb and the sub-agent list.
struct SwarmNodeTranscript {
    chat_view: WeakEntity<ChatView>,
    msg_idx: usize,
    name: String,
    /// The message box (TM-5).
    message: Entity<InputState>,
    /// What became of the last message sent from the box.
    sent: Option<SharedString>,
    _observe: Subscription,
    _message: Subscription,
}

impl SwarmNodeTranscript {
    fn new(
        chat_view: Entity<ChatView>,
        msg_idx: usize,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _observe = cx.observe(&chat_view, |_, _, cx| cx.notify());
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Message this agent…"));
        let _message = cx.subscribe_in(&message, window, |this, input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let text = input.read(cx).value().trim().to_string();
                if !text.is_empty() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    this.send(text, cx);
                }
            }
        });
        Self {
            chat_view: chat_view.downgrade(),
            msg_idx,
            name,
            message,
            sent: None,
            _observe,
            _message,
        }
    }

    /// Send `text` from the human to the agent this sheet shows: it reads
    /// it at its next tool call (TM-5).
    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let name = self.name.clone();
        let Some(broker) = swarm_broker(cx) else {
            self.sent = Some("No broker is running.".into());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let sent = match broker.post_message(&name, text).await {
                Ok(chatty_fabric::MessageStatus::Pending { .. }) => {
                    format!("Sent: {name} reads it at its next tool call.")
                }
                Ok(chatty_fabric::MessageStatus::Refused { reason }) => {
                    format!("{name} did not take the message: {reason}.")
                }
                Err(error) => format!("Could not message {name}: {error}"),
            };
            this.update(cx, |this, cx| {
                this.sent = Some(sent.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The message box, under a running agent's transcript.
    fn message_box(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "swarm-message-box".into())
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .pt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(Input::new(&self.message))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        self.sent.clone().unwrap_or_else(|| {
                            "It reads your message at its next tool call.".into()
                        }),
                    ),
            )
    }
}

impl Render for SwarmNodeTranscript {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = self
            .chat_view
            .upgrade()
            .and_then(|view| view.read(cx).run_tree(self.msg_idx));
        let found = tree.and_then(|tree| tree.index_of(&self.name).map(|ix| (tree, ix)));
        match found {
            Some((tree, ix)) => {
                let this = cx.entity().downgrade();
                let running = matches!(tree.nodes[ix].status, NodeStatus::Running);
                let transcript = AgentTranscript::new(tree, ix)
                    .on_stop(std::rc::Rc::new(|name, cx| stop_swarm_node(&name, cx)))
                    .on_navigate(std::rc::Rc::new(move |name, _window, cx| {
                        this.update(cx, |this, cx| {
                            this.name = name;
                            cx.notify();
                        })
                        .ok();
                    }));
                // The sheet's own body grows with its content instead of
                // scrolling, so after a few tool calls a box at the end of
                // the transcript sat below the sheet's bottom edge (AGE-839).
                // Taken out of the flow, this fills the body at the sheet's
                // height: the transcript scrolls, the box stays pinned under
                // it.
                v_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "swarm-transcript".into())
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(transcript),
                    )
                    .when(running, |this| this.child(self.message_box(cx)))
                    .into_any_element()
            }
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("This agent's run is no longer in the conversation.")
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    // Named imports, not `super::*`: `use gpui::*` would shadow `#[test]`.
    use super::ChatView;
    use crate::chatty::views::transcript::{SwarmNodeView, SwarmTree};
    use crate::settings::models::ExecutionSettingsModel;
    use chatty_core::services::swarm_trace::{NodeStatus, ToolCall, ToolOutcome};
    use gpui::{AppContext as _, Entity};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    /// A running worker with `calls` finished tool calls: a transcript far
    /// taller than the window.
    fn busy_worker(calls: usize) -> SwarmTree {
        let tool_calls = (0..calls)
            .map(|n| ToolCall {
                id: format!("c{n}"),
                name: "read_file".into(),
                plugin: None,
                arguments: None,
                outcome: ToolOutcome::Done {
                    result: format!("line {n} of the file"),
                },
            })
            .collect();
        SwarmTree {
            nodes: vec![SwarmNodeView {
                name: "analyst-0".into(),
                spec: "analyst".into(),
                model: None,
                status: NodeStatus::Running,
                depth: 0,
                parent: None,
                tokens: 0,
                cost: None,
                usage: Vec::new(),
                turns: calls as u32,
                text_bytes: 0,
                tool_calls,
            }],
            ..SwarmTree::default()
        }
    }

    /// AGE-839: after a few tool calls the message box sat at the end of the
    /// sheet's scrolling body, below the sheet's bottom edge. It must stay
    /// pinned inside the window, with the transcript scrolling above it.
    #[gpui::test]
    fn message_box_stays_reachable_with_many_tool_calls(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(crate::settings::models::general_model::GeneralSettingsModel::default());
            cx.set_global(ExecutionSettingsModel::default());
            cx.set_global(crate::settings::models::ExtensionsModel::default());
            cx.set_global(chatty_core::models::ErrorStore::new(100));
            cx.set_global(crate::auto_updater::AutoUpdater::new("0.0.0"));
            cx.set_global(chatty_core::models::ConversationsStore::new());
        });
        let slot: Rc<RefCell<Option<Entity<ChatView>>>> = Rc::default();
        let slot_for_window = slot.clone();
        let window = cx.add_window(move |window, cx| {
            let view = cx.new(|cx| ChatView::new(window, cx));
            *slot_for_window.borrow_mut() = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = slot.borrow_mut().take().expect("ChatView captured");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);

        vcx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.delegation_runs.insert(0, Arc::new(busy_worker(40)));
                view.open_swarm_node(0, "analyst-0".into(), window, cx);
            })
        });
        vcx.run_until_parked();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();

        let viewport = vcx.update(|window, _| window.viewport_size());
        let transcript = vcx
            .debug_bounds("swarm-transcript")
            .expect("the sheet draws the transcript");
        assert!(
            transcript.size.height > viewport.height,
            "40 tool calls must overflow the window for this test to mean anything: \
             transcript {transcript:?}, window {viewport:?}"
        );
        let message_box = vcx
            .debug_bounds("swarm-message-box")
            .expect("a running agent's sheet has a message box");
        assert!(
            message_box.origin.y >= gpui::px(0.)
                && message_box.bottom() <= viewport.height,
            "the message box must lie inside the window: box {message_box:?}, window {viewport:?}"
        );
    }
}
