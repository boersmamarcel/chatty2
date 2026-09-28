//! The chat view's half of the swarm tree (TB-4, AGE-666).
//!
//! `StreamManager` folds every event of a turn into a
//! [`SwarmTrace`] and hands it here each time the swarm changes. The view
//! remembers which delegation rows the running stream opened, cuts the
//! subtree under each row's callee out of the trace and stores it on the
//! row's `DisplayMessage`, where the adapter turns it into a
//! `Block::SwarmTree`. A row whose callee delegated to no one keeps the
//! plain delegation row it always had.
//!
//! A tree line opens that agent's transcript in a sheet. The sheet reads the
//! row's current tree each time it draws, so a transcript opened on a
//! running agent keeps up with it.

use std::sync::Arc;

use chatty_core::models::token_usage::PriceBook;
use chatty_core::services::swarm_trace::{NodeStatus, SwarmTrace};
use gpui::*;
use gpui_component::{ActiveTheme, WindowExt as _};

use super::ChatView;
use crate::chatty::views::transcript::{
    AgentTranscript, SwarmTree, adapt_swarm_tree, delegation_callees,
};

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
                continue;
            }
            let tree = adapt_swarm_tree(trace, top, book);
            let Some(msg) = self.messages.get_mut(*idx) else {
                continue;
            };
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
        for (idx, _) in &self.swarm_rows {
            let Some(msg) = self.messages.get_mut(*idx) else {
                continue;
            };
            let Some(tree) = msg.swarm_tree.as_ref().filter(|t| t.running() > 0) else {
                continue;
            };
            let mut settled = SwarmTree::clone(tree);
            for node in &mut settled.nodes {
                if node.status.is_running() {
                    node.status = NodeStatus::Canceled;
                }
            }
            msg.swarm_tree = Some(Arc::new(settled));
            changed = true;
        }
        if changed {
            cx.notify();
        }
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
        let title = name.clone();
        let transcript = cx.new(|cx| SwarmNodeTranscript::new(chat_view, msg_idx, name, cx));
        window.open_sheet(cx, move |sheet, _window, _cx| {
            sheet
                .title(title.clone())
                .size(px(TRANSCRIPT_SHEET_WIDTH))
                .child(transcript.clone())
        });
    }
}

/// The sheet's body: one agent's transcript, redrawn whenever the chat view
/// changes so a running agent's calls keep arriving.
struct SwarmNodeTranscript {
    chat_view: WeakEntity<ChatView>,
    msg_idx: usize,
    name: String,
    _observe: Subscription,
}

impl SwarmNodeTranscript {
    fn new(
        chat_view: Entity<ChatView>,
        msg_idx: usize,
        name: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let _observe = cx.observe(&chat_view, |_, _, cx| cx.notify());
        Self {
            chat_view: chat_view.downgrade(),
            msg_idx,
            name,
            _observe,
        }
    }
}

impl Render for SwarmNodeTranscript {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let node = self.chat_view.upgrade().and_then(|view| {
            view.read(cx)
                .messages
                .get(self.msg_idx)
                .and_then(|msg| msg.swarm_tree.as_ref())
                .and_then(|tree| tree.node(&self.name).cloned())
        });
        match node {
            Some(node) => AgentTranscript::new(node).into_any_element(),
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("This agent's run is no longer in the conversation.")
                .into_any_element(),
        }
    }
}
