//! "Add models" — browse a provider's catalogue instead of typing an
//! identifier.
//!
//! The sheet opens on the provider's live catalogue: search it, tick as many
//! models as you want, add them in one go. Models already in the roster show
//! as added and can't be picked twice. Manual identifier entry is still here,
//! folded into a disclosure, for anything the catalogue doesn't list.

use super::*;
use crate::settings::providers::openrouter::OpenRouterCatalog;
use chatty_core::settings::providers::openrouter::discovery::{
    OpenRouterModel, model_completion_cost, model_prompt_cost, model_supports_images,
    model_supports_pdf,
};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

/// One row of a provider catalogue, normalised out of whatever the provider
/// returned.
#[derive(Clone)]
struct CatalogEntry {
    identifier: String,
    name: String,
    context: Option<i32>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
    supports_images: bool,
    supports_pdf: bool,
}

impl CatalogEntry {
    fn from_openrouter(m: &OpenRouterModel) -> Self {
        Self {
            identifier: m.id.clone(),
            name: m.name.clone(),
            context: Some(m.context_length as i32),
            input_cost: model_prompt_cost(m),
            output_cost: model_completion_cost(m),
            supports_images: model_supports_images(m),
            supports_pdf: model_supports_pdf(m),
        }
    }

    fn into_config(self, provider_type: ProviderType) -> ModelConfig {
        let mut config = ModelConfig::new(
            uuid::Uuid::new_v4().to_string(),
            self.name,
            provider_type,
            self.identifier,
        );
        config.max_context_window = self.context;
        config.cost_per_million_input_tokens = self.input_cost;
        config.cost_per_million_output_tokens = self.output_cost;
        config.supports_images = self.supports_images;
        config.supports_pdf = self.supports_pdf;
        config
    }
}

/// What the sheet can show for a provider: a catalogue to browse, or a reason
/// there isn't one.
enum Catalog {
    Entries(Vec<CatalogEntry>),
    /// No catalogue to browse, and why — shown in place of the list.
    Unavailable(&'static str),
}

/// How many models a provider's catalogue holds, for the chip label. Cheap —
/// the chips render on every frame and don't need the entries themselves.
fn catalog_len(provider_type: &ProviderType, cx: &App) -> Option<usize> {
    match provider_type {
        ProviderType::OpenRouter => cx
            .try_global::<OpenRouterCatalog>()
            .map(|c| c.models.len())
            .filter(|n| *n > 0),
        ProviderType::Ollama | ProviderType::AzureOpenAI => None,
    }
}

fn catalog_for(provider_type: &ProviderType, cx: &App) -> Catalog {
    match provider_type {
        ProviderType::OpenRouter => match cx.try_global::<OpenRouterCatalog>() {
            Some(catalog) if !catalog.models.is_empty() => Catalog::Entries(
                catalog
                    .models
                    .iter()
                    .map(CatalogEntry::from_openrouter)
                    .collect(),
            ),
            // The catalogue is fetched by the startup sync, which needs a key.
            _ => Catalog::Unavailable(
                "No OpenRouter catalogue yet — add a key in Manage keys, then reopen this sheet.",
            ),
        },
        ProviderType::Ollama => Catalog::Unavailable(
            "Ollama models appear on their own as you pull them — no need to add them here.",
        ),
        ProviderType::AzureOpenAI => Catalog::Unavailable(
            "Azure serves your own deployments. Connect it in Manage keys, then add the deployment by name below.",
        ),
    }
}

impl ModelsListView {
    pub(super) fn show_add_model_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        trace!("Opening Add models sheet");

        // Dialog-local state. The dialog body is a closure re-run on every
        // redraw, so anything it mutates has to outlive a single pass.
        let active_provider = Rc::new(RefCell::new(ProviderType::OpenRouter));
        let selected: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
        let manual_open = Rc::new(Cell::new(false));

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let manual_name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g., GPT-4 Turbo"));
        let manual_identifier =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g., openai/gpt-4-turbo"));

        cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();

        let view = cx.entity();

        window.open_dialog(cx, move |dialog, _window, cx| {
            let colors = RosterColors::of(cx);

            let provider_type = active_provider.borrow().clone();
            let catalog = catalog_for(&provider_type, cx);

            // Identifiers already in the roster for this provider — shown as
            // added, and not selectable.
            let existing: HashSet<String> = cx
                .global::<ModelsModel>()
                .models()
                .iter()
                .filter(|m| m.provider_type == provider_type)
                .map(|m| m.model_identifier.clone())
                .collect();

            let query = search.read(cx).value().to_lowercase();
            let selected_count = selected.borrow().len();

            dialog
                .title("Add models")
                .overlay(true)
                .keyboard(true)
                .close_button(true)
                .overlay_closable(true)
                .w(px(700.))
                .child(
                    v_flex()
                        .child(
                            div()
                                .px_4()
                                .pb_3()
                                .text_xs()
                                .text_color(colors.muted_fg)
                                .child("Pick from a provider's catalogue — no identifiers to type"),
                        )
                        // ── Provider chips ────────────────────────────────
                        .child(
                            h_flex().px_4().pb_3().gap_2().children(
                                PROVIDERS
                                    .iter()
                                    .map(|candidate| {
                                        let candidate = candidate.clone();
                                        let is_active = candidate == provider_type;
                                        let count = match catalog_len(&candidate, cx) {
                                            Some(n) => format!(" · {n}"),
                                            None => String::new(),
                                        };
                                        let label =
                                            format!("{}{count}", candidate.display_name());
                                        let active_provider = active_provider.clone();

                                        chip(
                                            SharedString::from(format!(
                                                "add-provider-{}",
                                                candidate.display_name()
                                            )),
                                            is_active,
                                            colors,
                                            move |cx| {
                                                *active_provider.borrow_mut() = candidate.clone();
                                                cx.refresh_windows();
                                            },
                                        )
                                        .child(label)
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                        )
                        // ── Search ────────────────────────────────────────
                        .when(matches!(catalog, Catalog::Entries(_)), |this| {
                            this.child(div().px_4().pb_3().child(Input::new(&search).small()))
                        })
                        // ── Catalogue list ────────────────────────────────
                        .child(match &catalog {
                            Catalog::Unavailable(reason) => div()
                                .px_4()
                                .py_8()
                                .text_sm()
                                .text_color(colors.muted_fg)
                                .child(*reason)
                                .into_any_element(),
                            Catalog::Entries(entries) => {
                                let matching: Vec<&CatalogEntry> = entries
                                    .iter()
                                    .filter(|e| {
                                        query.is_empty()
                                            || e.name.to_lowercase().contains(&query)
                                            || e.identifier.to_lowercase().contains(&query)
                                    })
                                    // The full OpenRouter catalogue is hundreds
                                    // of rows; the search box is how you reach
                                    // the rest.
                                    .take(200)
                                    .collect();

                                div()
                                    .id("add-model-catalog")
                                    .h(px(280.))
                                    .overflow_y_scrollbar()
                                    .when(matching.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .px_4()
                                                .py_8()
                                                .text_sm()
                                                .text_color(colors.muted_fg)
                                                .child("No models match that search"),
                                        )
                                    })
                                    .children(
                                        matching
                                            .into_iter()
                                            .map(|entry| {
                                                let already_added =
                                                    existing.contains(&entry.identifier);
                                                let is_selected =
                                                    selected.borrow().contains(&entry.identifier);
                                                let identifier = entry.identifier.clone();
                                                let selected = selected.clone();

                                                h_flex()
                                                    .id(SharedString::from(format!(
                                                        "catalog-{identifier}"
                                                    )))
                                                    .h(px(44.))
                                                    .items_center()
                                                    .gap_3()
                                                    .px_4()
                                                    .border_b_1()
                                                    .border_color(colors.border)
                                                    .when(already_added, |this| this.opacity(0.5))
                                                    .when(!already_added, |this| {
                                                        this.cursor_pointer().on_mouse_down(
                                                            MouseButton::Left,
                                                            move |_, _, cx| {
                                                                let mut set = selected.borrow_mut();
                                                                if !set.remove(&identifier) {
                                                                    set.insert(identifier.clone());
                                                                }
                                                                drop(set);
                                                                cx.refresh_windows();
                                                            },
                                                        )
                                                    })
                                                    .when(is_selected, |this| this.bg(colors.accent_bg))
                                                    // Tick box
                                                    .child(
                                                        div()
                                                            .size(px(15.))
                                                            .rounded_sm()
                                                            .border_1()
                                                            .border_color(if is_selected {
                                                                colors.accent
                                                            } else {
                                                                colors.muted_fg
                                                            })
                                                            .when(
                                                                is_selected || already_added,
                                                                |this| {
                                                                    this.child(
                                                                        Icon::new(IconName::Check)
                                                                            .size_3()
                                                                            .text_color(colors.accent),
                                                                    )
                                                                },
                                                            ),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .text_sm()
                                                                    .text_color(colors.fg)
                                                                    .child(entry.name.clone())
                                                                    .when(already_added, |this| {
                                                                        this.child(
                                                                            div()
                                                                                .text_xs()
                                                                                .text_color(
                                                                                    colors.muted_fg,
                                                                                )
                                                                                .child(
                                                                                    "· already added",
                                                                                ),
                                                                        )
                                                                    }),
                                                            )
                                                            .child(
                                                                div()
                                                                    .truncate()
                                                                    .text_xs()
                                                                    .text_color(colors.muted_fg)
                                                                    .child(entry.identifier.clone()),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .w(px(64.))
                                                            .text_xs()
                                                            .text_color(colors.muted_fg)
                                                            .child(format_context(entry.context)),
                                                    )
                                                    .child(
                                                        div()
                                                            .w(px(64.))
                                                            .text_xs()
                                                            .text_color(colors.muted_fg)
                                                            .child(match entry.input_cost {
                                                                Some(c) => format!("${c:.2}"),
                                                                None => "—".to_string(),
                                                            }),
                                                    )
                                            })
                                            .collect::<Vec<_>>(),
                                    )
                                    .into_any_element()
                            }
                        })
                        // ── Manual entry disclosure ───────────────────────
                        .child(
                            v_flex()
                                .px_4()
                                .py_2()
                                .border_t_1()
                                .border_color(colors.border)
                                .child(
                                    h_flex()
                                        .id("manual-entry-toggle")
                                        .gap_1()
                                        .items_center()
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(colors.accent)
                                        .child(
                                            Icon::new(if manual_open.get() {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .size_3(),
                                        )
                                        .child("Enter an identifier manually")
                                        .on_mouse_down(MouseButton::Left, {
                                            let manual_open = manual_open.clone();
                                            move |_, _, cx| {
                                                manual_open.set(!manual_open.get());
                                                cx.refresh_windows();
                                            }
                                        }),
                                )
                                .when(manual_open.get(), |this| {
                                    this.child(
                                        v_flex()
                                            .pt_2()
                                            .gap_2()
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(colors.muted_fg)
                                                            .child("Display name"),
                                                    )
                                                    .child(Input::new(&manual_name).small()),
                                            )
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(colors.muted_fg)
                                                            .child("Model identifier"),
                                                    )
                                                    .child(Input::new(&manual_identifier).small()),
                                            ),
                                    )
                                }),
                        )
                        // ── Footer ────────────────────────────────────────
                        .child(
                            h_flex()
                                .px_4()
                                .py_3()
                                .gap_3()
                                .items_center()
                                .border_t_1()
                                .border_color(colors.border)
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(colors.muted_fg)
                                        .child(if selected_count > 0 {
                                            format!(
                                                "{selected_count} selected · added as enabled, not favourites"
                                            )
                                        } else {
                                            "Nothing selected yet".to_string()
                                        }),
                                )
                                .child(
                                    Button::new("add-model-cancel")
                                        .label("Cancel")
                                        .small()
                                        .outline()
                                        .on_click(|_, window, cx| {
                                            window.close_dialog(cx);
                                        }),
                                )
                                .child({
                                    let selected = selected.clone();
                                    let manual_name = manual_name.clone();
                                    let manual_identifier = manual_identifier.clone();
                                    let manual_open = manual_open.clone();
                                    let provider_type = provider_type.clone();
                                    let view = view.clone();
                                    let entries = match &catalog {
                                        Catalog::Entries(entries) => entries.clone(),
                                        Catalog::Unavailable(_) => Vec::new(),
                                    };
                                    let manual_id_value =
                                        manual_identifier.read(cx).value().trim().to_string();
                                    let manual_ready =
                                        manual_open.get() && !manual_id_value.is_empty();

                                    Button::new("add-model-confirm")
                                        .label(if selected_count > 0 {
                                            format!("Add {selected_count} models")
                                        } else {
                                            "Add model".to_string()
                                        })
                                        .small()
                                        .primary()
                                        .disabled(selected_count == 0 && !manual_ready)
                                        .on_click(move |_, window, cx| {
                                            let picks = selected.borrow().clone();
                                            for entry in
                                                entries.iter().filter(|e| picks.contains(&e.identifier))
                                            {
                                                models_controller::create_model(
                                                    entry.clone().into_config(provider_type.clone()),
                                                    cx,
                                                );
                                            }

                                            // Manual entry, when the disclosure is open and filled.
                                            let identifier =
                                                manual_identifier.read(cx).value().trim().to_string();
                                            if manual_open.get() && !identifier.is_empty() {
                                                let name = manual_name.read(cx).value().trim().to_string();
                                                let name = if name.is_empty() {
                                                    identifier.clone()
                                                } else {
                                                    name
                                                };
                                                models_controller::create_model(
                                                    ModelConfig::new(
                                                        uuid::Uuid::new_v4().to_string(),
                                                        name,
                                                        provider_type.clone(),
                                                        identifier,
                                                    ),
                                                    cx,
                                                );
                                            }

                                            selected.borrow_mut().clear();
                                            window.close_dialog(cx);
                                            view.update(cx, |view, cx| view.refresh(cx));
                                        })
                                }),
                        ),
                )
        });
    }
}
