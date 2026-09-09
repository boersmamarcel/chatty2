//! Settings → Models & Providers.
//!
//! One roster for both: a provider status strip on top, then every model as a
//! dense row with its favourite, default, context, price and temperature
//! visible without opening anything. Replaces the old split between a Models
//! page and a Providers page.
//!
//! # What lives here
//!
//! - `ModelsListView` — the roster: filtering, sorting, and the row rendering.
//! - `add_model_sheet` — browse a provider's catalogue and add in one shot.
//! - `provider_keys_sheet` — one sheet for every provider's credentials.
//! - `dialogs` — the per-model edit form, reached from a row's ⋯ menu.
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
use gpui::{
    AnyElement, App, Context, Corner, Entity, FocusHandle, Focusable, Hsla, IntoElement,
    MouseButton, Render, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement,
    select::{Select, SelectState},
    tab::{Tab, TabBar},
    v_flex,
};
use tracing::trace;

// Global state to store the models list view
pub type GlobalModelsListView = crate::global_entity::GlobalStrongEntity<ModelsListView>;

/// Map a provider's display name back to its type — the edit form's provider
/// select works in display names.
fn string_to_provider_type(s: &str) -> ProviderType {
    match s {
        "Ollama" => ProviderType::Ollama,
        "Azure OpenAI" => ProviderType::AzureOpenAI,
        _ => ProviderType::OpenRouter,
    }
}

/// The providers the roster knows about, in the order their chips appear.
const PROVIDERS: [ProviderType; 3] = [
    ProviderType::OpenRouter,
    ProviderType::Ollama,
    ProviderType::AzureOpenAI,
];

/// Column widths, shared by the header row and every model row so the two
/// stay in step.
mod col {
    use gpui::{Pixels, px};

    pub const STAR: Pixels = px(26.);
    pub const PROVIDER: Pixels = px(92.);
    pub const CONTEXT: Pixels = px(68.);
    pub const PRICE: Pixels = px(70.);
    pub const TEMP: Pixels = px(54.);
    pub const MENU: Pixels = px(26.);
}

/// The quick filter chips above the roster.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RosterFilter {
    All,
    Favourites,
    Local,
}

impl RosterFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Favourites => "Favourites",
            Self::Local => "Local",
        }
    }

    fn matches(self, model: &ModelConfig) -> bool {
        match self {
            Self::All => true,
            Self::Favourites => model.is_favorite,
            Self::Local => model.provider_type == ProviderType::Ollama,
        }
    }
}

/// Roster sort order. Clicking the sort control cycles through these.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Provider,
    Context,
    Price,
}

impl SortKey {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Provider => "provider",
            Self::Context => "context",
            Self::Price => "price",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Name => Self::Provider,
            Self::Provider => Self::Context,
            Self::Context => Self::Price,
            Self::Price => Self::Name,
        }
    }
}

/// The theme colours the roster and its sheets draw with, resolved once per
/// render and passed down rather than re-read (and re-borrowed) per element.
#[derive(Clone, Copy)]
pub(super) struct RosterColors {
    pub fg: Hsla,
    pub muted_fg: Hsla,
    pub border: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub accent_bg: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
}

impl RosterColors {
    pub(super) fn of(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            fg: theme.foreground,
            muted_fg: theme.muted_foreground,
            border: theme.border,
            muted: theme.muted,
            accent: theme.primary,
            accent_bg: theme.accent,
            success: theme.success,
            warning: theme.warning,
            danger: theme.danger,
        }
    }
}

/// How a provider is doing, for its chip in the status strip.
struct ProviderStatus {
    provider_type: ProviderType,
    configured: bool,
    model_count: usize,
}

impl ProviderStatus {
    fn detail(&self) -> String {
        if self.configured {
            format!("{}", self.model_count)
        } else {
            "needs key".to_string()
        }
    }
}

pub struct ModelsListView {
    focus_handle: FocusHandle,
    search: Entity<InputState>,
    filter: RosterFilter,
    /// `None` shows every provider; otherwise only the chosen one.
    provider_filter: Option<ProviderType>,
    sort: SortKey,
}

impl ModelsListView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter models"));

        // Re-render as the query changes; the roster is derived in render()
        // straight from the global store, so there is no cache to invalidate.
        cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            search,
            filter: RosterFilter::All,
            provider_filter: None,
            sort: SortKey::Name,
        }
    }

    /// Kept for callers that mutate models and then ask for a redraw. The
    /// roster reads the store on every render, so a notify is all it takes.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Provider chips: configured state and how many models each contributes.
    fn provider_statuses(cx: &App) -> Vec<ProviderStatus> {
        let configured: Vec<ProviderType> = cx
            .global::<ProviderModel>()
            .configured_providers()
            .map(|p| p.provider_type.clone())
            .collect();
        let models = cx.global::<ModelsModel>().models();

        PROVIDERS
            .iter()
            .map(|provider_type| ProviderStatus {
                provider_type: provider_type.clone(),
                configured: configured.contains(provider_type),
                model_count: models
                    .iter()
                    .filter(|m| &m.provider_type == provider_type)
                    .count(),
            })
            .collect()
    }

    /// The rows to draw: search, chip filter and provider filter applied,
    /// favourites pinned above the rest, then the chosen sort within each half.
    fn visible_models(&self, cx: &App) -> Vec<ModelConfig> {
        let query = self.search.read(cx).value().to_lowercase();

        let mut models: Vec<ModelConfig> = cx
            .global::<ModelsModel>()
            .models()
            .iter()
            .filter(|m| self.filter.matches(m))
            .filter(|m| {
                self.provider_filter
                    .as_ref()
                    .is_none_or(|p| &m.provider_type == p)
            })
            .filter(|m| {
                query.is_empty()
                    || m.name.to_lowercase().contains(&query)
                    || m.model_identifier.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();

        let sort = self.sort;
        models.sort_by(|a, b| {
            // Favourites pin to the top whatever the sort key is.
            b.is_favorite
                .cmp(&a.is_favorite)
                .then_with(|| match sort {
                    SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    SortKey::Provider => a
                        .provider_type
                        .display_name()
                        .cmp(b.provider_type.display_name()),
                    // Biggest context and priciest first — the interesting end.
                    SortKey::Context => b.max_context_window.cmp(&a.max_context_window),
                    SortKey::Price => b
                        .cost_per_million_input_tokens
                        .partial_cmp(&a.cost_per_million_input_tokens)
                        .unwrap_or(std::cmp::Ordering::Equal),
                })
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        models
    }
}

impl Focusable for ModelsListView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// `200000` → `200K`, `1000000` → `1M`. Context windows are read at a glance,
/// so the exact digits are noise.
fn format_context(tokens: Option<i32>) -> String {
    match tokens {
        Some(t) if t >= 1_000_000 => {
            let m = t as f32 / 1_000_000.0;
            if (m.fract() * 10.0).round() == 0.0 {
                format!("{m:.0}M")
            } else {
                format!("{m:.1}M")
            }
        }
        Some(t) if t >= 1_000 => format!("{}K", t / 1_000),
        Some(t) => t.to_string(),
        None => "—".to_string(),
    }
}

/// Input-token price per million, or `free` for anything running locally.
fn format_price(model: &ModelConfig) -> String {
    match model.cost_per_million_input_tokens {
        Some(cost) => format!("${cost:.2}"),
        None if model.provider_type == ProviderType::Ollama => "free".to_string(),
        None => "—".to_string(),
    }
}

/// A pill — the `Default` and `Local` markers on a row.
fn pill(label: impl Into<SharedString>, text: Hsla, bg: Hsla) -> impl IntoElement {
    div()
        .px_2()
        .rounded_full()
        .bg(bg)
        .text_xs()
        .text_color(text)
        .child(label.into())
}

/// A filter chip. `on_click` fires on press; `active` paints the selected state.
fn chip(
    id: impl Into<gpui::ElementId>,
    active: bool,
    colors: RosterColors,
    on_click: impl Fn(&mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(id)
        .h(px(26.))
        .px_3()
        .gap_2()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .text_xs()
        .when(active, |this| {
            this.bg(colors.accent_bg).text_color(colors.accent)
        })
        .when(!active, |this| this.text_color(colors.muted_fg))
        .hover(move |this| this.text_color(colors.fg))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| on_click(cx))
}

impl Render for ModelsListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = RosterColors::of(cx);

        let statuses = Self::provider_statuses(cx);
        let models = self.visible_models(cx);
        let total = cx.global::<ModelsModel>().models().len();
        let favourites = cx
            .global::<ModelsModel>()
            .models()
            .iter()
            .filter(|m| m.is_favorite)
            .count();
        let configured_count = statuses.iter().filter(|s| s.configured).count();

        let entity = cx.entity();

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            // ── Header ────────────────────────────────────────────────────
            .child(
                h_flex()
                    .justify_between()
                    .items_end()
                    .gap_4()
                    .pb_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_lg().child("Models & Providers"))
                            .child(
                                div().text_xs().text_color(colors.muted_fg).child(format!(
                                    "{total} models across {configured_count} providers · {favourites} favourites"
                                )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("manage-keys-btn")
                                    .label("Manage keys")
                                    .small()
                                    .outline()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_provider_keys_sheet(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("add-model-btn")
                                    .label("Add model")
                                    .icon(Icon::new(IconName::Plus))
                                    .small()
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        trace!("Add model button clicked");
                                        this.show_add_model_sheet(window, cx);
                                    })),
                            ),
                    ),
            )
            // ── Provider status strip ─────────────────────────────────────
            .child(
                h_flex().gap_2().pb_3().children(
                    statuses
                        .iter()
                        .map(|status| {
                            let provider_type = status.provider_type.clone();
                            let selected = self.provider_filter.as_ref() == Some(&provider_type);
                            let entity = entity.clone();
                            let dot = if status.configured { colors.success } else { colors.warning };

                            chip(
                                SharedString::from(format!(
                                    "provider-chip-{}",
                                    provider_type.display_name()
                                )),
                                selected,
                                colors,
                                move |cx| {
                                    let provider_type = provider_type.clone();
                                    entity.update(cx, |this, cx| {
                                        // Clicking the active chip clears the filter.
                                        this.provider_filter = if this.provider_filter.as_ref()
                                            == Some(&provider_type)
                                        {
                                            None
                                        } else {
                                            Some(provider_type)
                                        };
                                        cx.notify();
                                    });
                                },
                            )
                            .child(
                                div()
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(dot),
                            )
                            .child(format!(
                                "{} · {}",
                                status.provider_type.display_name(),
                                status.detail()
                            ))
                        })
                        .collect::<Vec<_>>(),
                ),
            )
            // ── Filter bar ────────────────────────────────────────────────
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .pb_2()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(div().w(px(240.)).child(Input::new(&self.search).small()))
                    .children(
                        [RosterFilter::All, RosterFilter::Favourites, RosterFilter::Local]
                            .into_iter()
                            .map(|filter| {
                                let entity = entity.clone();
                                chip(
                                    SharedString::from(format!("filter-{}", filter.label())),
                                    self.filter == filter,
                                    colors,
                                    move |cx| {
                                        entity.update(cx, |this, cx| {
                                            this.filter = filter;
                                            cx.notify();
                                        });
                                    },
                                )
                                .child(filter.label())
                            })
                            .collect::<Vec<_>>(),
                    )
                    .child(
                        h_flex().ml_auto().child({
                            let entity = entity.clone();
                            let label = format!("Sort: {}", self.sort.label());
                            Button::new("sort-btn")
                                .label(label)
                                .icon(Icon::new(IconName::ChevronDown))
                                .small()
                                .ghost()
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.sort = this.sort.next();
                                        cx.notify();
                                    });
                                })
                        }),
                    ),
            )
            // ── Column header ─────────────────────────────────────────────
            .child(
                h_flex()
                    .h(px(30.))
                    .items_center()
                    .px_3()
                    .text_xs()
                    .text_color(colors.muted_fg)
                    .child(div().w(col::STAR))
                    .child(div().flex_1().min_w_0().child("Model"))
                    .child(div().w(col::PROVIDER).child("Provider"))
                    .child(div().w(col::CONTEXT).child("Context"))
                    .child(div().w(col::PRICE).child("$ / 1M in"))
                    .child(div().w(col::TEMP).child("Temp"))
                    .child(div().w(col::MENU)),
            )
            // ── Rows ──────────────────────────────────────────────────────
            .child(
                div()
                    .id("models-roster")
                    .flex_1()
                    .min_h(px(240.))
                    .overflow_y_scrollbar()
                    .when(models.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .py_8()
                                .gap_1()
                                .items_center()
                                .child(div().text_color(colors.muted_fg).child(if total == 0 {
                                    "No models yet — add one to get started"
                                } else {
                                    "No models match these filters"
                                })),
                        )
                    })
                    .children(
                        models
                            .iter()
                            .map(|model| {
                                Self::render_row(model, colors, &entity)
                            })
                            .collect::<Vec<_>>(),
                    ),
            )
            // ── Footer ────────────────────────────────────────────────────
            .child(
                h_flex()
                    .justify_between()
                    .pt_2()
                    .border_t_1()
                    .border_color(colors.border)
                    .text_xs()
                    .text_color(colors.muted_fg)
                    .child(format!("Showing {} of {}", models.len(), total))
                    .child("Changes save automatically"),
            )
    }
}

impl ModelsListView {
    /// One roster row: favourite star, name + markers + identifier, then the
    /// numbers, then the ⋯ menu.
    /// Takes the view entity rather than a context: the row is built inside a
    /// `map` closure, and an element borrowing `cx` can't escape it.
    fn render_row(model: &ModelConfig, colors: RosterColors, entity: &Entity<Self>) -> AnyElement {
        let model_id = model.id.clone();
        let is_favorite = model.is_favorite;
        let entity = entity.clone();

        h_flex()
            .id(SharedString::from(format!("row-{}", model.id)))
            .h(px(46.))
            .items_center()
            .px_3()
            .text_sm()
            .border_b_1()
            .border_color(colors.border)
            .when(model.is_default, |this| this.bg(colors.accent_bg))
            // Star
            .child(
                div()
                    .id(SharedString::from(format!("star-{}", model.id)))
                    .w(col::STAR)
                    .cursor_pointer()
                    .text_color(if is_favorite {
                        colors.accent
                    } else {
                        colors.muted_fg
                    })
                    .child(if is_favorite { "★" } else { "☆" })
                    .on_mouse_down(MouseButton::Left, {
                        let model_id = model_id.clone();
                        move |_, _, cx| {
                            models_controller::toggle_favorite(&model_id, cx);
                        }
                    }),
            )
            // Name, markers, identifier
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(div().text_color(colors.fg).child(model.name.clone()))
                    .when(model.is_default, |this| {
                        this.child(pill("Default", colors.accent, colors.muted))
                    })
                    .when(model.provider_type == ProviderType::Ollama, |this| {
                        this.child(pill("Local", colors.muted_fg, colors.muted))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(colors.muted_fg)
                            .child(model.model_identifier.clone()),
                    ),
            )
            .child(
                div()
                    .w(col::PROVIDER)
                    .text_xs()
                    .text_color(colors.muted_fg)
                    .child(model.provider_type.display_name().to_string()),
            )
            .child(
                div()
                    .w(col::CONTEXT)
                    .text_xs()
                    .text_color(colors.muted_fg)
                    .child(format_context(model.max_context_window)),
            )
            .child(
                div()
                    .w(col::PRICE)
                    .text_xs()
                    .text_color(colors.muted_fg)
                    .child(format_price(model)),
            )
            .child(
                div()
                    .w(col::TEMP)
                    .text_xs()
                    .text_color(colors.muted_fg)
                    .child(format!("{:.1}", model.temperature)),
            )
            // Row menu
            .child(
                div().w(col::MENU).child(
                    Button::new(SharedString::from(format!("row-menu-{}", model.id)))
                        .icon(Icon::new(IconName::Ellipsis))
                        .xsmall()
                        .ghost()
                        .dropdown_menu_with_anchor(Corner::TopRight, {
                            let model_id = model_id.clone();
                            let entity = entity.clone();
                            let is_default = model.is_default;
                            move |menu, _, _| {
                                let set_default_id = model_id.clone();
                                let edit_id = model_id.clone();
                                let delete_id = model_id.clone();
                                let entity_for_edit = entity.clone();
                                let entity_for_delete = entity.clone();

                                menu.item(
                                    PopupMenuItem::new("Set as default")
                                        .checked(is_default)
                                        .on_click(move |_, _, cx| {
                                            models_controller::set_default_model(
                                                &set_default_id,
                                                cx,
                                            );
                                        }),
                                )
                                .item(PopupMenuItem::new("Edit…").on_click(move |_, window, cx| {
                                    let edit_id = edit_id.clone();
                                    entity_for_edit.update(cx, |view, cx| {
                                        view.show_edit_model_dialog(edit_id, window, cx);
                                    });
                                }))
                                .item(
                                    PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                                        models_controller::delete_model(delete_id.clone(), cx);
                                        entity_for_delete.update(cx, |view, cx| {
                                            view.refresh(cx);
                                        });
                                    }),
                                )
                            }
                        }),
                ),
            )
            .into_any_element()
    }
}

mod add_model_sheet;
mod dialogs;
mod provider_keys_sheet;
