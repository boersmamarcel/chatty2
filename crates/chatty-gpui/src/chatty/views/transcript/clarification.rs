use std::collections::HashMap;
use std::sync::Arc;

use chatty_core::models::clarification_store::ClarifyingQuestion;
use chatty_core::models::message_types::{ClarificationBlock, ClarificationState};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, Sizable};

/// Which answer the user has picked for one question.
///
/// `Custom` is not stored explicitly — a non-empty free-text box always wins
/// over a clicked option, so the card only tracks clicked options here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChosenOption(pub usize);

pub type ChoiceCallback = Arc<dyn Fn(String, usize, &mut App) + Send + Sync>;
pub type SubmitCallback = Arc<dyn Fn(&mut App) + Send + Sync>;

/// The live popover shown just above the chat input while the agent waits for
/// the user to answer its clarifying questions.
#[derive(IntoElement)]
pub struct ClarificationCard {
    id: String,
    questions: Vec<ClarifyingQuestion>,
    choices: HashMap<String, ChosenOption>,
    /// One free-text input per question, positioned by question index.
    custom_inputs: Vec<Entity<InputState>>,
    can_submit: bool,
    on_choose: Option<ChoiceCallback>,
    on_submit: Option<SubmitCallback>,
}

impl ClarificationCard {
    pub fn new(
        id: impl Into<String>,
        questions: Vec<ClarifyingQuestion>,
        choices: HashMap<String, ChosenOption>,
        custom_inputs: Vec<Entity<InputState>>,
        can_submit: bool,
    ) -> Self {
        Self {
            id: id.into(),
            questions,
            choices,
            custom_inputs,
            can_submit,
            on_choose: None,
            on_submit: None,
        }
    }

    pub fn on_choose<F>(mut self, f: F) -> Self
    where
        F: Fn(String, usize, &mut App) + Send + Sync + 'static,
    {
        self.on_choose = Some(Arc::new(f));
        self
    }

    pub fn on_submit<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_submit = Some(Arc::new(f));
        self
    }
}

impl RenderOnce for ClarificationCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let card_id = self.id.clone();
        let on_choose = self.on_choose;
        let on_submit = self.on_submit;
        let multiple = self.questions.len() > 1;

        div()
            .id(ElementId::Name(format!("clarification-{card_id}").into()))
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if multiple {
                        format!("{} questions before continuing", self.questions.len())
                    } else {
                        "A question before continuing".to_string()
                    }),
            )
            .children(
                self.questions
                    .iter()
                    .enumerate()
                    .map(|(q_ix, question)| {
                        let chosen = self.choices.get(&question.id).copied();
                        let input = self.custom_inputs.get(q_ix).cloned();

                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child(question.question.clone()),
                            )
                            .child(div().flex().flex_row().flex_wrap().gap_2().children(
                                question.options.iter().enumerate().map(|(opt_ix, option)| {
                                    let selected = chosen == Some(ChosenOption(opt_ix));
                                    let button = Button::new(ElementId::Name(
                                        format!("clarify-{card_id}-{q_ix}-{opt_ix}").into(),
                                    ))
                                    .small()
                                    .label(option.clone());

                                    let button = if selected {
                                        button.primary()
                                    } else {
                                        button.outline()
                                    };

                                    button.on_click({
                                        let on_choose = on_choose.clone();
                                        let qid = question.id.clone();
                                        move |_, _, cx| {
                                            if let Some(cb) = &on_choose {
                                                cb(qid.clone(), opt_ix, cx);
                                            }
                                        }
                                    })
                                }),
                            ))
                            .when_some(input, |this, input| this.child(Input::new(&input).small()))
                    })
                    .collect::<Vec<_>>(),
            )
            .child(
                div().flex().flex_row().justify_end().child(
                    Button::new(ElementId::Name(format!("clarify-submit-{card_id}").into()))
                        .primary()
                        .small()
                        .label("Send answer")
                        .disabled(!self.can_submit)
                        .on_click(move |_, _, cx| {
                            if let Some(cb) = &on_submit {
                                cb(cx);
                            }
                        }),
                ),
            )
    }
}

/// The settled record of a clarification, rendered inside the transcript once
/// the user has answered (or the request lapsed).
#[derive(IntoElement)]
pub struct ClarificationSummary {
    block: ClarificationBlock,
}

impl ClarificationSummary {
    pub fn new(block: ClarificationBlock) -> Self {
        Self { block }
    }
}

impl RenderOnce for ClarificationSummary {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.block.id.clone();
        let answers: HashMap<&str, &str> = self
            .block
            .answers
            .iter()
            .map(|a| (a.id.as_str(), a.answer.as_str()))
            .collect();

        let heading = match self.block.state {
            ClarificationState::Pending => "Waiting for your answer",
            ClarificationState::Answered => "You answered",
            ClarificationState::Cancelled => "Unanswered",
        };

        div()
            .id(ElementId::Name(
                format!("clarification-summary-{id}").into(),
            ))
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(heading),
            )
            .children(
                self.block
                    .questions
                    .iter()
                    .map(|question| {
                        let answer = answers.get(question.id.as_str()).copied();
                        div()
                            .text_xs()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(question.question.clone()),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().foreground)
                                    .child(answer.unwrap_or("—").to_string()),
                            )
                    })
                    .collect::<Vec<_>>(),
            )
    }
}
