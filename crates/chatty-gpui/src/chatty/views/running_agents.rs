//! The running-agents overview (TB-6, AGE-748): every live agent across
//! conversations, with its status, elapsed time and spend, a jump to its
//! transcript and a Stop.
//!
//! [`RunningAgentsModel`] listens to the `StreamManager` on its own: each
//! `SwarmTreeChanged` hands it a conversation's latest swarm, whichever
//! conversation is on screen, and the turn's `StreamEnded` drops it. The
//! rows are `chatty_core`'s [`RunningAgents`]; nothing here polls the
//! broker. It notifies on every change, for the open panel, and emits
//! [`RunningAgentsEvent::CountsChanged`] only when the footer chip's
//! numbers move, so the app redraws for the chip and nothing else.
//!
//! The chip ([`RunningAgentsChip`]) opens the panel ([`RunningAgentsPanel`])
//! in a side sheet. A row jumps to its conversation and opens the agent's
//! transcript sheet ([`RunningAgentTranscript`]), read from the model's copy
//! of that conversation's swarm, so it works whichever conversation was on
//! screen. Stop goes through the desktop's broker like the tree's (TB-7).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chatty_core::services::running_agents::{
    Activity, RunningAgent, RunningAgents, format_elapsed, spend_text,
};
use chatty_core::services::swarm_trace::SwarmTrace;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, WindowExt as _, h_flex, v_flex};

use crate::chatty::controllers::GlobalChattyApp;
use crate::chatty::models::{ConversationsStore, GlobalStreamManager, StreamManagerEvent};
use crate::chatty::views::chat_view::stop_swarm_node;
use crate::chatty::views::sidebar_view::SidebarEvent;
use crate::chatty::views::transcript::{AgentTranscript, SwarmTree, adapt_swarm_tree};
use crate::settings::models::models_store::ModelsModel;

const PANEL_WIDTH: f32 = 560.;
const TRANSCRIPT_WIDTH: f32 = 520.;
/// How often an open panel redraws its elapsed-time column. Only the clock
/// moves on it: the rows themselves change on events.
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// The footer chip's numbers moved.
pub enum RunningAgentsEvent {
    CountsChanged,
}

/// Every conversation's live swarm, kept from the `StreamManager`'s events.
pub struct RunningAgentsModel {
    agents: RunningAgents,
    /// Live agents, and how many of them wait on the human.
    counts: (usize, usize),
    _streams: Option<Subscription>,
}

impl EventEmitter<RunningAgentsEvent> for RunningAgentsModel {}

impl RunningAgentsModel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let _streams = cx
            .try_global::<GlobalStreamManager>()
            .and_then(|global| global.get())
            .map(|manager| {
                cx.subscribe(&manager, |this, manager, event, cx| match event {
                    StreamManagerEvent::SwarmTreeChanged {
                        conversation_id,
                        trace,
                    } => this.update(conversation_id, trace.clone(), cx),
                    StreamManagerEvent::StreamEnded {
                        conversation_id,
                        epoch,
                        ..
                    } => {
                        // A superseded stream's end is not this turn's
                        // (AGE-151).
                        if manager.read(cx).is_current_epoch(conversation_id, *epoch) {
                            this.end(conversation_id, cx);
                        }
                    }
                    _ => {}
                })
            });
        Self {
            agents: RunningAgents::new(),
            counts: (0, 0),
            _streams,
        }
    }

    /// `conversation_id`'s swarm is now `trace`.
    pub fn update(
        &mut self,
        conversation_id: &str,
        trace: Arc<SwarmTrace>,
        cx: &mut Context<Self>,
    ) {
        let title = cx
            .try_global::<ConversationsStore>()
            .and_then(|store| store.get_conversation(conversation_id))
            .map(|conversation| conversation.title().to_string())
            .unwrap_or_else(|| "New Chat".to_string());
        self.agents.update(conversation_id, &title, trace);
        self.changed(cx);
    }

    /// `conversation_id`'s turn ended.
    pub fn end(&mut self, conversation_id: &str, cx: &mut Context<Self>) {
        if self.agents.end(conversation_id) {
            self.changed(cx);
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        let counts = self.agents.counts();
        if counts != self.counts {
            self.counts = counts;
            cx.emit(RunningAgentsEvent::CountsChanged);
        }
        cx.notify();
    }

    pub fn rows(&self) -> Vec<RunningAgent> {
        self.agents.rows()
    }

    /// Live agents, and how many of them wait on the human.
    pub fn counts(&self) -> (usize, usize) {
        self.counts
    }

    /// `agent`'s run in `conversation_id`, as the tree's transcript draws
    /// it: the subtree of the root's callee it runs under.
    fn run_tree(&self, conversation_id: &str, agent: &str, cx: &App) -> Option<Arc<SwarmTree>> {
        let trace = self.agents.trace(conversation_id)?;
        let tree = trace.tree();
        let mut top = trace.node_named(agent)?;
        while let Some(parent) = tree.parent(top).filter(|&p| p != tree.root()) {
            top = parent;
        }
        let book = cx
            .try_global::<ModelsModel>()
            .map(|models| models.price_book())
            .unwrap_or_default();
        Some(Arc::new(adapt_swarm_tree(trace, top, &book)))
    }
}

/// The footer chip: `N running`, amber with the waiting count when one of
/// them waits on you. Nothing while no agent runs.
#[derive(IntoElement)]
pub struct RunningAgentsChip {
    model: Entity<RunningAgentsModel>,
}

impl RunningAgentsChip {
    pub fn new(model: Entity<RunningAgentsModel>) -> Self {
        Self { model }
    }
}

impl RenderOnce for RunningAgentsChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (running, waiting) = self.model.read(cx).counts();
        let color = if waiting > 0 {
            cx.theme().warning
        } else {
            cx.theme().primary
        };
        let label = if waiting > 0 {
            format!("{running} running \u{b7} {waiting} waiting")
        } else {
            format!("{running} running")
        };
        let model = self.model;
        div().when(running > 0, |this| {
            this.child(
                Button::new("running-agents-chip")
                    .ghost()
                    .xsmall()
                    .tooltip("Every running agent, across conversations")
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(Icon::new(IconName::Loader).size(px(12.)).text_color(color))
                            .child(div().text_xs().text_color(color).child(label)),
                    )
                    .on_click(move |_, window, cx| open_panel(model.clone(), window, cx)),
            )
        })
    }
}

/// Open the running-agents panel in a side sheet.
pub fn open_panel(model: Entity<RunningAgentsModel>, window: &mut Window, cx: &mut App) {
    let panel = cx.new(|cx| RunningAgentsPanel::new(model, cx));
    window.open_sheet(cx, move |sheet, _window, _cx| {
        sheet
            .title("Running agents")
            .size(px(PANEL_WIDTH))
            .child(panel.clone())
    });
}

/// The panel's body: one row per live agent.
pub struct RunningAgentsPanel {
    model: Entity<RunningAgentsModel>,
    _observe: Subscription,
    _clock: Task<()>,
}

impl RunningAgentsPanel {
    fn new(model: Entity<RunningAgentsModel>, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&model, |_, _, cx| cx.notify());
        let _clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            model,
            _observe,
            _clock,
        }
    }
}

impl Render for RunningAgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.model.read(cx).rows();
        let book = cx
            .try_global::<ModelsModel>()
            .map(|models| models.price_book())
            .unwrap_or_default();
        let now = SystemTime::now();
        if rows.is_empty() {
            return div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No agents are running.")
                .into_any_element();
        }
        v_flex()
            .gap_1()
            .children(rows.into_iter().enumerate().map(|(ix, row)| {
                let spend = spend_text(&row.spend, Some(&book));
                agent_row(ix, row, spend, now, self.model.clone(), cx)
            }))
            .into_any_element()
    }
}

fn agent_row(
    ix: usize,
    row: RunningAgent,
    spend: String,
    now: SystemTime,
    model: Entity<RunningAgentsModel>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let status = match row.activity {
        Activity::Running => Tag::info().small().child(row.activity.label()),
        Activity::Waiting(_) => Tag::warning().small().child(row.activity.label()),
    };
    let elapsed = format_elapsed(row.elapsed(now));
    let context = format!("{} \u{b7} {}", row.conversation, row.chain_text());
    let (conversation_id, agent) = (row.conversation_id.clone(), row.agent.clone());
    let stop_agent = row.agent.clone();
    h_flex()
        .id(ElementId::Name(format!("running-agent-{ix}").into()))
        .w_full()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|style| style.bg(theme.muted))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(row.agent.clone()),
                        )
                        .child(status),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(context),
                ),
        )
        .child(
            v_flex()
                .items_end()
                .gap_0p5()
                .child(div().text_xs().text_color(theme.foreground).child(elapsed))
                .child(div().text_xs().text_color(theme.muted_foreground).child(spend)),
        )
        .child(
            Button::new(ElementId::Name(format!("running-agent-stop-{ix}").into()))
                .ghost()
                .xsmall()
                .label("Stop")
                .tooltip("Stop this agent and every agent it started")
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    stop_swarm_node(&stop_agent, cx);
                }),
        )
        .on_click(move |_, window, cx| {
            jump_to(model.clone(), &conversation_id, &agent, window, cx);
        })
        .into_any_element()
}

/// Show `conversation_id` and open `agent`'s transcript over it.
fn jump_to(
    model: Entity<RunningAgentsModel>,
    conversation_id: &str,
    agent: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let active = cx
        .try_global::<ConversationsStore>()
        .and_then(|store| store.active_id().cloned());
    if active.as_deref() != Some(conversation_id)
        && let Some(app) = cx
            .try_global::<GlobalChattyApp>()
            .and_then(|global| global.try_upgrade())
    {
        let id = conversation_id.to_string();
        app.update(cx, |app, cx| {
            app.sidebar_view.update(cx, |_, cx| {
                cx.emit(SidebarEvent::SelectConversation(id));
            });
        });
    }
    let transcript = cx.new(|cx| {
        RunningAgentTranscript::new(model, conversation_id.to_string(), agent.to_string(), cx)
    });
    window.open_sheet(cx, move |sheet, _window, _cx| {
        sheet
            .title("Agent transcript")
            .size(px(TRANSCRIPT_WIDTH))
            .child(transcript.clone())
    });
}

/// One live agent's transcript, read from the model's copy of its
/// conversation's swarm and redrawn as it changes. After the turn ends it
/// keeps the last tree it drew.
pub struct RunningAgentTranscript {
    model: Entity<RunningAgentsModel>,
    conversation_id: String,
    name: String,
    last: Option<Arc<SwarmTree>>,
    _observe: Subscription,
}

impl RunningAgentTranscript {
    fn new(
        model: Entity<RunningAgentsModel>,
        conversation_id: String,
        name: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let _observe = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            conversation_id,
            name,
            last: None,
            _observe,
        }
    }
}

impl Render for RunningAgentTranscript {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(tree) = self
            .model
            .read(cx)
            .run_tree(&self.conversation_id, &self.name, cx)
        {
            self.last = Some(tree);
        }
        let found = self
            .last
            .clone()
            .and_then(|tree| tree.index_of(&self.name).map(|ix| (tree, ix)));
        match found {
            Some((tree, ix)) => {
                let this = cx.entity().downgrade();
                AgentTranscript::new(tree, ix)
                    .on_stop(std::rc::Rc::new(|name, cx| stop_swarm_node(&name, cx)))
                    .on_navigate(std::rc::Rc::new(move |name, _window, cx| {
                        this.update(cx, |this, cx| {
                            this.name = name;
                            cx.notify();
                        })
                        .ok();
                    }))
                    .into_any_element()
            }
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("This agent is no longer running.")
                .into_any_element(),
        }
    }
}
