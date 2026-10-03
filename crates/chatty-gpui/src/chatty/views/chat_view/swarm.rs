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
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, WindowExt as _};

use super::ChatView;
use crate::chatty::views::transcript::{
    AgentTranscript, SwarmFold, SwarmTree, adapt_swarm_tree, delegation_callees,
};

/// Stop `name` — a node of the running swarm, or the spec of the root's
/// own callee — and everything under it, through the desktop's broker
/// (TB-7). The rest of the swarm, and the turn, keep running.
pub(super) fn stop_swarm_node(name: &str, cx: &mut App) {
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
                    .child(self.sent.clone().unwrap_or_else(|| {
                        "It reads your message at its next tool call.".into()
                    })),
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
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(transcript)
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
