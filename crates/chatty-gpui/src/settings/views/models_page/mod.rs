//! Settings → Models page.
//!
//! Lets users browse, add, edit, and delete LLM model configurations,
//! grouped by provider. Includes the Azure OpenAI deployment-id flow and
//! the OpenRouter curated catalog picker.
//!
//! # What lives here
//!
//! - `ModelsPage` view + render path.
//! - Add / edit modal flows for each provider type (Anthropic, OpenAI,
//!   Azure, Ollama, Mistral, Gemini, OpenRouter, …).
//! - Capability toggles (image / PDF / temperature) per model.
//! - OpenRouter catalog search & one-click import.
//!
//! # What does NOT live here
//!
//! - The underlying data model — `chatty_core::settings::models::models_store::ModelConfig`.
//! - Persistence — `chatty_core::settings::repositories::models_repository`.
//! - Capability defaults per provider — `ProviderType::default_capabilities`.
//! - The actual LLM agent construction — `chatty_core::factories::agent_factory`.

use crate::settings::controllers::models_controller;
use crate::settings::models::models_store::{AZURE_DEFAULT_API_VERSION, ModelConfig, ModelsModel};
use crate::settings::models::providers_store::{ProviderModel, ProviderType};
use crate::settings::providers::openrouter::OpenRouterCatalog;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, MouseButton, Render, Styled, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IndexPath, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    list::{List, ListDelegate, ListItem, ListState},
    scroll::ScrollableElement,
    select::{Select, SelectEvent, SelectState},
    tab::{Tab, TabBar},
    v_flex,
};
use tracing::trace;

// Global state to store the models list view
pub type GlobalModelsListView = crate::global_entity::GlobalStrongEntity<ModelsListView>;

// Helper function to convert provider display name to ProviderType
fn string_to_provider_type(s: &str) -> ProviderType {
    match s {
        "OpenRouter" => ProviderType::OpenRouter,
        "Ollama" => ProviderType::Ollama,
        "Azure OpenAI" => ProviderType::AzureOpenAI,
        _ => ProviderType::OpenRouter, // Default fallback
    }
}

pub struct ModelsListView {
    focus_handle: FocusHandle,
    list_state: Entity<ListState<ModelsListDelegate>>,
}

impl ModelsListView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let delegate = ModelsListDelegate::new(cx);
        let list_state = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));

        Self {
            focus_handle,
            list_state,
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.list_state.update(cx, |state, cx| {
            state.delegate_mut().reload(cx);
            cx.notify();
        });
    }
}


impl Focusable for ModelsListView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelsListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        v_flex()
            .size_full()
            .gap_4()
            .track_focus(&self.focus_handle)
            .child(
                // Header with title and add button
                h_flex()
                    .justify_between()
                    .items_center()
                    .pb_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex().gap_1().child(
                            div()
                                .text_2xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("AI Models"),
                        ),
                    )
                    .child(
                        Button::new("add-model-btn")
                            .label("+ Add Model")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                trace!("Add Model button clicked");
                                this.show_add_model_dialog(window, cx);
                            })),
                    ),
            )
            .child(
                // List container
                div()
                    .flex_1()
                    .w_full()
                    .child(List::new(&self.list_state).max_h(px(600.)).min_w(px(500.))),
            )
    }
}


mod delegate;
mod dialogs;

use delegate::ModelsListDelegate;
