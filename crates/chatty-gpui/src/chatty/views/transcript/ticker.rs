use std::collections::VecDeque;
use std::time::Duration;

use gpui::*;
use gpui_component::ActiveTheme;

use super::{HEADLINE_QUEUE_MAX, HEADLINE_TICK_MS};

/// Throttled headline queue shared by ActivityGroup and the running indicator.
pub struct HeadlineTicker {
    queue: VecDeque<String>,
    current: Option<String>,
    generation: u64,
    timer_started: bool,
}

impl HeadlineTicker {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            queue: VecDeque::new(),
            current: None,
            generation: 0,
            timer_started: false,
        }
    }

    pub fn push(&mut self, headline: impl Into<String>, cx: &mut Context<Self>) {
        let headline = headline.into();
        if self.queue.back().is_some_and(|h| h == &headline)
            || self.current.as_ref() == Some(&headline)
        {
            return;
        }
        if self.queue.len() >= HEADLINE_QUEUE_MAX {
            self.queue.pop_front();
        }
        self.queue.push_back(headline);
        if self.current.is_none() {
            self.advance(cx);
        }
        cx.notify();
    }

    fn advance(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.queue.pop_front() {
            self.current = Some(next);
            self.generation = self.generation.wrapping_add(1);
            cx.notify();
        }
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(HEADLINE_TICK_MS))
                    .await;
                if entity
                    .update(cx, |this, cx| {
                        if !this.queue.is_empty() {
                            this.advance(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Render for HeadlineTicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule(cx);
        let label = self
            .current
            .clone()
            .unwrap_or_else(|| "Working".to_string());
        div()
            .id(ElementId::NamedInteger(
                "headline-ticker".into(),
                self.generation,
            ))
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
    }
}

pub fn new_headline_ticker(cx: &mut App) -> Entity<HeadlineTicker> {
    cx.new(HeadlineTicker::new)
}
