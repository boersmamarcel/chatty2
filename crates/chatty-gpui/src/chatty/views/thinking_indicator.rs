//! Running indicator: terracotta asterisk on two periods, shared headline ticker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, App, AppContext, Context, Entity, IntoElement, ParentElement, Render,
    Styled, Window, div, px, radians,
};
use gpui_component::{ActiveTheme as _, Icon, IconName};

use super::transcript::{GLYPH_OPACITY_MS, GLYPH_ROTATE_MS, HeadlineTicker};

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

static START_OFFSET_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn next_start_offset() -> usize {
    START_OFFSET_COUNTER.fetch_add(1, Ordering::Relaxed) % THINKING_WORDS.len()
}

pub struct ThinkingIndicator {
    start_offset: usize,
    started_at: Instant,
    tick: usize,
    timer_started: bool,
    ticker: Entity<HeadlineTicker>,
    attention: String,
    steps_done: usize,
    steps_total: usize,
}

impl ThinkingIndicator {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let ticker = cx.new(HeadlineTicker::new);
        Self {
            start_offset: next_start_offset(),
            started_at: Instant::now(),
            tick: 0,
            timer_started: false,
            ticker,
            attention: String::new(),
            steps_done: 0,
            steps_total: 0,
        }
    }

    pub fn set_attention(&mut self, attention: impl Into<String>, cx: &mut Context<Self>) {
        let next = attention.into();
        if next != self.attention {
            self.attention = next;
            cx.notify();
        }
    }

    pub fn set_progress(&mut self, done: usize, total: usize, cx: &mut Context<Self>) {
        if done != self.steps_done || total != self.steps_total {
            self.steps_done = done;
            self.steps_total = total;
            cx.notify();
        }
    }

    #[allow(dead_code)]
    pub fn ticker(&self) -> Entity<HeadlineTicker> {
        self.ticker.clone()
    }

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
                cx.background_executor()
                    .timer(Duration::from_millis(GLYPH_OPACITY_MS))
                    .await;
                if entity
                    .update(cx, |this, cx| {
                        this.tick = this.tick.wrapping_add(1);
                        let word = this.current_word();
                        this.ticker.update(cx, |ticker, cx| {
                            ticker.push(format!("{word}…"), cx);
                        });
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn current_word(&self) -> &'static str {
        let steps = (self.started_at.elapsed().as_millis() as u64 / GLYPH_ROTATE_MS) as usize;
        let idx = (self.start_offset + steps) % THINKING_WORDS.len();
        THINKING_WORDS[idx]
    }
}

impl Render for ThinkingIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_tick(cx);

        let primary = cx.theme().primary;
        let muted = cx.theme().muted_foreground;
        let elapsed = self.started_at.elapsed().as_secs();
        let elapsed_label = if elapsed >= 1 {
            format!(" · {elapsed}s")
        } else {
            String::new()
        };
        let word = self.current_word();
        let phrase = if self.attention.is_empty() {
            format!("{word}{elapsed_label}")
        } else {
            format!("{word} {}{elapsed_label}", self.attention)
        };
        let (pip_filled, step_label) = if self.steps_total > 0 {
            let filled = ((self.steps_done * 7) / self.steps_total.max(1)).clamp(1, 7);
            (
                filled,
                format!("{} of {} steps", self.steps_done, self.steps_total),
            )
        } else {
            (((self.tick % 7) + 1), String::new())
        };

        // 1a: terracotta asterisk, attention phrase, seven step pips on one row.
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .child(
                div()
                    .child(
                        Icon::new(IconName::Asterisk)
                            .text_color(primary)
                            .with_animation(
                                "running-glyph-rotate",
                                Animation::new(Duration::from_millis(GLYPH_ROTATE_MS)).repeat(),
                                |this, delta| this.rotate(radians(delta * std::f32::consts::TAU)),
                            ),
                    )
                    .with_animation(
                        "running-glyph-opacity",
                        Animation::new(Duration::from_millis(GLYPH_OPACITY_MS)).repeat(),
                        |this, delta| {
                            let wave = (delta * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                            this.opacity(0.45 + 0.55 * wave)
                        },
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(phrase)
                    .with_animation(
                        gpui::ElementId::NamedInteger("thinking-word".into(), self.tick as u64),
                        Animation::new(Duration::from_millis(400)),
                        |this, delta| this.opacity(0.4 + 0.6 * delta),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(3.))
                    .children((0..7).map(move |i| {
                        let on = i < pip_filled;
                        let pip = div().w(px(10.)).h(px(7.)).rounded_sm().bg(if on {
                            primary
                        } else {
                            muted.opacity(0.35)
                        });
                        if i + 1 == pip_filled {
                            pip.with_animation(
                                gpui::ElementId::NamedInteger("running-pip".into(), i as u64),
                                Animation::new(Duration::from_millis(GLYPH_OPACITY_MS)).repeat(),
                                |this, delta| {
                                    let wave = (delta * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                                    this.opacity(0.55 + 0.45 * wave)
                                },
                            )
                            .into_any_element()
                        } else {
                            pip.into_any_element()
                        }
                    })),
            )
            .when(!step_label.is_empty(), |this| {
                this.child(div().text_xs().text_color(muted).child(step_label))
            })
    }
}

pub fn new_thinking_indicator(cx: &mut App) -> Entity<ThinkingIndicator> {
    cx.new(ThinkingIndicator::new)
}
