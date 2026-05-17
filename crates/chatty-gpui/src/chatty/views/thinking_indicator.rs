//! Animated "thinking" indicator shown in [`ChatView`] while the
//! assistant has not yet produced its first token (the empty-streaming
//! state replaced by the loading skeleton).
//!
//! It combines a [`gpui_component::spinner::Spinner`] with a label that
//! cycles through a small set of playful verbs every ~2 seconds. The
//! intent (modeled after Claude Code, Cursor, Aider, etc.) is to give
//! the user a constant signal that the agent is doing something even
//! when no streaming text is arriving (typical while tools execute).
//!
//! The entity owns its own background timer so it keeps animating even
//! when no stream events are firing — `ChatView` would otherwise only
//! re-render when text/tool chunks arrive, leaving the indicator
//! visually frozen during long silent tool calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, App, AppContext, Context, Entity, IntoElement, ParentElement, Render,
    Styled, Window, div,
};
use gpui_component::{ActiveTheme as _, Sizable, Size, spinner::Spinner};

/// Rotating verbs shown next to the spinner. Order is random per stream
/// to keep the experience fresh; keep the list short so users see each
/// one occasionally rather than always the same first three.
const THINKING_WORDS: &[&str] = &[
    "Thinking",
    "Pondering",
    "Cogitating",
    "Reasoning",
    "Hatching",
    "Brewing",
    "Plotting",
    "Stitching",
    "Untangling",
    "Wrangling",
    "Noodling",
    "Tinkering",
    "Cooking",
    "Conjuring",
    "Crunching",
    "Spelunking",
    "Wiring",
    "Sketching",
    "Marinating",
    "Percolating",
];

/// How often the label rotates to the next word.
const ROTATE_INTERVAL: Duration = Duration::from_millis(1800);

/// Process-wide monotonic counter that picks the starting word for each
/// new indicator (or `reset()` call). Using a counter instead of an RNG
/// keeps the dependency footprint zero while still ensuring that
/// consecutive turns visibly start on a different word.
static START_OFFSET_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn next_start_offset() -> usize {
    START_OFFSET_COUNTER.fetch_add(1, Ordering::Relaxed) % THINKING_WORDS.len()
}

pub struct ThinkingIndicator {
    /// Starting offset into `THINKING_WORDS` so two consecutive streams
    /// don't always begin with "Thinking".
    start_offset: usize,
    /// Wall-clock time the indicator was first shown — drives the
    /// "Xs" elapsed counter so the user gets a sense of progress.
    started_at: Instant,
    /// Number of `tick()` calls so far. We rotate the word on each
    /// tick, and the tick is scheduled on the background executor at
    /// `ROTATE_INTERVAL`.
    tick: usize,
    /// Whether the timer loop is running. Once started, the entity
    /// reschedules itself every tick until dropped.
    timer_started: bool,
}

impl ThinkingIndicator {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            start_offset: next_start_offset(),
            started_at: Instant::now(),
            tick: 0,
            timer_started: false,
        }
    }

    /// Reset the elapsed counter and pick a fresh starting word. Called
    /// when a new assistant turn begins so the "Xs" counter doesn't
    /// keep counting from the previous turn.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.start_offset = next_start_offset();
        self.started_at = Instant::now();
        self.tick = 0;
        cx.notify();
    }

    fn schedule_tick(&mut self, cx: &mut Context<Self>) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor().timer(ROTATE_INTERVAL).await;
                if entity
                    .update(cx, |this, cx| {
                        this.tick = this.tick.wrapping_add(1);
                        cx.notify();
                    })
                    .is_err()
                {
                    // Entity dropped — stop the loop.
                    break;
                }
            }
        })
        .detach();
    }

    fn current_word(&self) -> &'static str {
        let idx = (self.start_offset + self.tick) % THINKING_WORDS.len();
        THINKING_WORDS[idx]
    }
}

impl Render for ThinkingIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Start (or keep) the rotation timer on first render. Render is
        // the natural place to do this because the entity is created
        // before being added to the view tree.
        self.schedule_tick(cx);

        let muted = cx.theme().muted_foreground;
        let elapsed = self.started_at.elapsed().as_secs();
        let elapsed_label = if elapsed >= 1 {
            format!(" · {elapsed}s")
        } else {
            String::new()
        };
        let word = self.current_word();

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .py_2()
            .text_color(muted)
            .child(Spinner::new().with_size(Size::Small).color(muted))
            .child(
                // Wrap label in a key'd animation so each word change
                // gently fades in instead of swapping abruptly.
                div()
                    .text_sm()
                    .child(format!("{word}…{elapsed_label}"))
                    .with_animation(
                        gpui::ElementId::NamedInteger("thinking-word".into(), self.tick as u64),
                        Animation::new(Duration::from_millis(400)),
                        |this, delta| this.opacity(0.4 + 0.6 * delta),
                    ),
            )
    }
}

/// Convenience constructor used by `chat_view`.
pub fn new_thinking_indicator(cx: &mut App) -> Entity<ThinkingIndicator> {
    cx.new(ThinkingIndicator::new)
}
