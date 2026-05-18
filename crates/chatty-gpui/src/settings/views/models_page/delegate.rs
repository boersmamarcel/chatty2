//! `ModelsListDelegate` — the `ListDelegate` impl backing the models page list.
//!
//! Sections the model list by provider type, handles search filtering, and
//! renders each row. Kept separate from `ModelsListView` so the view file
//! reads top-to-bottom as one rendered surface.

use super::*;

pub(super) struct ModelsListDelegate {
    sections: Vec<(ProviderType, Vec<ModelConfig>)>,
    selected_index: Option<IndexPath>,
    all_models: Vec<ModelConfig>,
    search_query: String,
}

impl ModelsListDelegate {
    pub(super) fn new(cx: &mut App) -> Self {
        let mut delegate = Self {
            sections: Vec::new(),
            selected_index: None,
            all_models: Vec::new(),
            search_query: String::new(),
        };
        delegate.reload(cx);
        delegate
    }

    pub(super) fn reload(&mut self, cx: &mut App) {
        let models = cx.global::<ModelsModel>().models().to_vec();
        self.all_models = models;
        self.rebuild_sections();
    }

    fn rebuild_sections(&mut self) {
        self.sections.clear();

        let provider_types = vec![
            ProviderType::OpenRouter,
            ProviderType::Ollama,
            ProviderType::AzureOpenAI,
        ];

        for provider_type in provider_types {
            let mut provider_models: Vec<ModelConfig> = self
                .all_models
                .iter()
                .filter(|m| m.provider_type == provider_type)
                .cloned()
                .collect();

            // Apply search filter
            if !self.search_query.is_empty() {
                let query = self.search_query.to_lowercase();
                provider_models.retain(|m| {
                    m.name.to_lowercase().contains(&query)
                        || m.model_identifier.to_lowercase().contains(&query)
                });
            }

            // Only add section if it has models
            if !provider_models.is_empty() {
                self.sections.push((provider_type, provider_models));
            }
        }
    }

    fn get_model(&self, ix: IndexPath) -> Option<&ModelConfig> {
        self.sections
            .get(ix.section)
            .and_then(|(_, models)| models.get(ix.row))
    }
}

impl ListDelegate for ModelsListDelegate {
    type Item = ListItem;

    fn sections_count(&self, _cx: &App) -> usize {
        self.sections.len()
    }

    fn items_count(&self, section: usize, _cx: &App) -> usize {
        self.sections
            .get(section)
            .map(|(_, models)| models.len())
            .unwrap_or(0)
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let (provider_type, models) = self.sections.get(section)?;
        let provider_name = provider_type.display_name().to_string();
        let count = models.len();
        let theme = cx.theme();

        Some(
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .bg(theme.background)
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(provider_name),
                )
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(theme.muted)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}", count)),
                ),
        )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let model = self.get_model(ix)?.clone();
        let model_id = model.id.clone();
        let model_id_for_delete = model.id.clone();
        let is_selected = Some(ix) == self.selected_index;
        let theme = cx.theme();

        // Create unique IDs for buttons using computed row index
        let row_index = ix.section * 1000 + ix.row;

        Some(
            ListItem::new(ix)
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(model.name.clone()),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new(("edit-model", row_index))
                                                .label("Edit")
                                                .small()
                                                .outline()
                                                .on_click(move |_, window, cx| {
                                                    // Access global view and show edit dialog
                                                    let view_entity = cx
                                                        .try_global::<GlobalModelsListView>()
                                                        .and_then(|g| g.get());

                                                    if let Some(view) = view_entity {
                                                        view.update(cx, |view, inner_cx| {
                                                            view.show_edit_model_dialog(
                                                                model_id.clone(),
                                                                window,
                                                                inner_cx,
                                                            );
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new(("delete-model", row_index))
                                                .label("Delete")
                                                .small()
                                                .outline()
                                                .on_click(move |_, _, cx| {
                                                    // Delete from store
                                                    models_controller::delete_model(
                                                        model_id_for_delete.clone(),
                                                        cx,
                                                    );

                                                    let view_entity = cx
                                                        .try_global::<GlobalModelsListView>()
                                                        .and_then(|g| g.get());

                                                    if let Some(view) = view_entity {
                                                        view.update(cx, |view, cx| {
                                                            view.refresh(cx);
                                                        });
                                                    }
                                                }),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("Model: {}", model.model_identifier)),
                        )
                        .child(
                            h_flex()
                                .gap_4()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format!("Temperature: {:.1}", model.temperature))
                                .when_some(model.max_tokens, |this, max_tokens| {
                                    this.child(format!("Max tokens: {}", max_tokens))
                                })
                                .when_some(model.top_p, |this, top_p| {
                                    this.child(format!("Top P: {:.2}", top_p))
                                }),
                        ),
                )
                .selected(is_selected)
                .px_3()
                .py_3(),
        )
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let message = if self.search_query.is_empty() {
            "No models configured yet"
        } else {
            "No models match your search"
        };

        v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .gap_2()
            .py_8()
            .child(
                div()
                    .text_lg()
                    .text_color(theme.muted_foreground)
                    .child(message),
            )
            .when(!self.search_query.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground.opacity(0.7))
                        .child("Try adjusting your search terms"),
                )
            })
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = ix;
        cx.notify();
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> gpui::Task<()> {
        self.search_query = query.to_string();
        self.rebuild_sections();
        cx.notify();
        gpui::Task::ready(())
    }
}
