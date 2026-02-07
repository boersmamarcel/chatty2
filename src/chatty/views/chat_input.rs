use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::popover::Popover;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, warn};

use crate::chatty::services::render_pdf_thumbnail;
use super::attachment_validation::{validate_attachment, is_image_extension, PDF_EXTENSION};



/// Callback type for sending messages (with attachments)
pub type SendMessageCallback =
    Arc<dyn Fn(String, Vec<PathBuf>, &mut Context<ChatInputState>) + Send + Sync>;

/// Callback type for model selection changes
pub type ModelChangeCallback = Arc<dyn Fn(String, &mut Context<ChatInputState>) + Send + Sync>;

/// State for the chat input component
pub struct ChatInputState {
    pub input: Entity<InputState>,
    attachments: Vec<PathBuf>,
    should_clear: bool,
    on_send: Option<SendMessageCallback>,
    on_model_change: Option<ModelChangeCallback>,
    selected_model_id: Option<String>,
    available_models: Vec<(String, String)>, // (id, display_name)
    supports_images: bool,
    supports_pdf: bool,
}

impl ChatInputState {
    pub fn new(input: Entity<InputState>) -> Self {
        Self {
            input,
            attachments: Vec::new(),
            should_clear: false,
            on_send: None,
            on_model_change: None,
            selected_model_id: None,
            available_models: Vec::new(),
            supports_images: false,
            supports_pdf: false,
        }
    }

    /// Set the callback for sending messages
    pub fn set_on_send<F>(&mut self, callback: F)
    where
        F: Fn(String, Vec<PathBuf>, &mut Context<ChatInputState>) + Send + Sync + 'static,
    {
        self.on_send = Some(Arc::new(callback));
    }

    /// Set the callback for model selection changes
    pub fn set_on_model_change<F>(&mut self, callback: F)
    where
        F: Fn(String, &mut Context<ChatInputState>) + Send + Sync + 'static,
    {
        self.on_model_change = Some(Arc::new(callback));
    }

    /// Set available models for selection
    pub fn set_available_models(
        &mut self,
        models: Vec<(String, String)>,
        default_id: Option<String>,
    ) {
        self.available_models = models;

        if self.selected_model_id.is_none() {
            self.selected_model_id =
                default_id.or_else(|| self.available_models.first().map(|(id, _)| id.clone()));
        }
    }

    /// Get the available models list
    pub fn available_models(&self) -> &[(String, String)] {
        &self.available_models
    }

    /// Set the selected model ID
    pub fn set_selected_model_id(&mut self, model_id: String) {
        self.selected_model_id = Some(model_id);
    }

    /// Set model capabilities for the currently selected model
    pub fn set_capabilities(&mut self, supports_images: bool, supports_pdf: bool) {
        self.supports_images = supports_images;
        self.supports_pdf = supports_pdf;
    }

    /// Add file attachments with validation
    pub fn add_attachments(&mut self, paths: Vec<PathBuf>, _cx: &mut Context<Self>) {
        for path in paths {
            if self.attachments.contains(&path) {
                warn!(?path, "File already attached");
                continue;
            }

            match validate_attachment(&path) {
                Ok(()) => {
                    self.attachments.push(path);
                }
                Err(err) => {
                    warn!(?path, ?err, "File validation failed");
                }
            }
        }
    }

    /// Remove attachment by index
    pub fn remove_attachment(&mut self, index: usize) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
        }
    }

    /// Get current attachments
    pub fn get_attachments(&self) -> &[PathBuf] {
        &self.attachments
    }

    /// Clear all attachments
    pub fn clear_attachments(&mut self) {
        self.attachments.clear();
    }

    /// Send the current message
    pub fn send_message(&mut self, cx: &mut Context<Self>) {
        let message = self.input.read(cx).text().to_string();
        let attachments = self.attachments.clone();

        debug!(message = %message, attachment_count = attachments.len(), "send_message called");

        if message.trim().is_empty() && attachments.is_empty() {
            warn!("Message is empty and no attachments, not sending");
            return;
        }

        if let Some(on_send) = &self.on_send {
            debug!("on_send callback exists, calling it");
            on_send(message, attachments, cx);
        } else {
            error!("on_send callback is NOT set");
        }

        self.should_clear = true;
        self.clear_attachments();
        debug!("Marked input for clearing");
    }

    /// Clear the input if needed
    pub fn clear_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_clear {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.should_clear = false;
        }
    }

    /// Get the selected model ID
    pub fn selected_model_id(&self) -> Option<&String> {
        self.selected_model_id.as_ref()
    }

    /// Get display name for selected model
    pub fn get_selected_model_display_name(&self) -> String {
        self.selected_model_id
            .as_ref()
            .and_then(|id| {
                self.available_models
                    .iter()
                    .find(|(model_id, _)| model_id == id)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_else(|| {
                if self.available_models.is_empty() {
                    "No models".to_string()
                } else {
                    "Select Model".to_string()
                }
            })
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .map(|ext| is_image_extension(&ext.to_string_lossy()))
        .unwrap_or(false)
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase() == PDF_EXTENSION)
        .unwrap_or(false)
}

fn render_file_chip(
    path: &Path,
    index: usize,
    state: &Entity<ChatInputState>,
) -> impl IntoElement {
    let state = state.clone();

    // Generate thumbnail path: actual image path for images, PDF thumbnail for PDFs
    let display_path = if is_image(path) {
        Some(path.to_path_buf())
    } else if is_pdf(path) {
        match render_pdf_thumbnail(path) {
            Ok(thumbnail_path) => Some(thumbnail_path),
            Err(e) => {
                warn!(?path, error = %e, "Failed to generate PDF thumbnail");
                None
            }
        }
    } else {
        None
    };

    div()
        .relative()
        .w_16()
        .h_16()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded_md()
        .when_some(display_path.clone(), |div, img_path| {
            div.child(
                img(img_path)
                    .w_full()
                    .h_full()
                    .object_fit(gpui::ObjectFit::Cover),
            )
        })
        .when(display_path.is_none(), |d| {
            d.child(
                div()
                    .w_full()
                    .h_full()
                    .bg(rgb(0xe5e7eb))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(0x6b7280))
                    .child("PDF"),
            )
        })
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .w_5()
                .h_5()
                .bg(rgb(0x374151))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(rgb(0xffffff))
                .text_xs()
                .hover(|style| style.bg(rgb(0x111827)))
                .child("×")
                .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                    state.update(cx, |state, _cx| {
                        state.remove_attachment(index);
                    });
                }),
        )
}

/// Chat input component for rendering
#[derive(IntoElement)]
pub struct ChatInput {
    state: Entity<ChatInputState>,
}

impl ChatInput {
    pub fn new(state: Entity<ChatInputState>) -> Self {
        Self { state }
    }
}

impl RenderOnce for ChatInput {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state_for_send = self.state.clone();
        let state_for_model = self.state.clone();
        let state_for_image = self.state.clone();
        let state_for_pdf = self.state.clone();
        let input_entity = self.state.read(cx).input.clone();

        // Read capabilities and attachments
        let supports_images = self.state.read(cx).supports_images;
        let supports_pdf = self.state.read(cx).supports_pdf;
        let show_attachment_button = supports_images || supports_pdf;
        let attachments = self.state.read(cx).get_attachments().to_vec();

        // Model display name
        let model_display = self.state.read(cx).get_selected_model_display_name();
        let _no_models = self.state.read(cx).available_models.is_empty();

        // Model dropdown button
        let model_button = Button::new("model-select").label(model_display.clone());

        // Model popover
        let model_popover = Popover::new("model-menu")
            .trigger(model_button)
            .appearance(false)
            .content(move |_, _window, cx| {
                let state = state_for_model.clone();
                let models = state.read(cx).available_models.clone();
                let selected_id = state.read(cx).selected_model_id.clone();

                div()
                    .flex()
                    .flex_col()
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .shadow_md()
                    .p_1()
                    .min_w(px(200.0))
                    .when(models.is_empty(), |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(rgb(0x6b7280))
                                .child("No Models Available"),
                        )
                    })
                    .when(!models.is_empty(), |d| {
                        d.children(models.iter().map(|(id, name)| {
                            let id_clone = id.clone();
                            let state_for_click = state.clone();
                            let is_selected = selected_id.as_ref() == Some(id);

                            div()
                                .px_3()
                                .py_2()
                                .rounded_sm()
                                .cursor_pointer()
                                .when(is_selected, |d| d.bg(cx.theme().secondary))
                                .hover(|style| style.bg(cx.theme().secondary))
                                .text_sm()
                                .child(name.clone())
                                .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                                    state_for_click.update(cx, |s, cx| {
                                        s.selected_model_id = Some(id_clone.clone());

                                        if let Some(on_change) = &s.on_model_change {
                                            on_change(id_clone.clone(), cx);
                                        }

                                        cx.notify();
                                    });
                                })
                        }))
                    })
            });

        // Attachment button with popover (only shown when model supports it)
        let attachment_popover = if show_attachment_button {
            let attach_button = Button::new("attach").label("+").tooltip("Add attachments");

            Some(
                Popover::new("attachment-menu")
                    .trigger(attach_button)
                    .appearance(false)
                    .content(move |_, _window, cx| {
                        let state_img = state_for_image.clone();
                        let state_pdf = state_for_pdf.clone();

                        div()
                            .flex()
                            .flex_col()
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .shadow_md()
                            .p_1()
                            .when(supports_images, |d| {
                                d.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(cx.theme().secondary))
                                        .text_sm()
                                        .child("Image")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_event, _window, cx| {
                                                let state = state_img.clone();
                                                cx.spawn(async move |cx| {
                                                    let receiver = cx
                                                        .update(|cx| {
                                                            cx.prompt_for_paths(
                                                                PathPromptOptions {
                                                                    files: true,
                                                                    directories: false,
                                                                    multiple: true,
                                                                    prompt: Some(
                                                                        "Select Images".into(),
                                                                    ),
                                                                },
                                                            )
                                                        })
                                                        .ok()?;

                                                    if let Ok(Some(paths)) =
                                                        receiver.await.ok()?
                                                    {
                                                        state
                                                            .update(cx, |state, cx| {
                                                                state.add_attachments(paths, cx);
                                                            })
                                                            .ok()?;
                                                    }
                                                    Some(())
                                                })
                                                .detach();
                                            },
                                        ),
                                )
                            })
                            .when(supports_pdf, |d| {
                                d.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(cx.theme().secondary))
                                        .text_sm()
                                        .child("PDF")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_event, _window, cx| {
                                                let state = state_pdf.clone();
                                                cx.spawn(async move |cx| {
                                                    let receiver = cx
                                                        .update(|cx| {
                                                            cx.prompt_for_paths(
                                                                PathPromptOptions {
                                                                    files: true,
                                                                    directories: false,
                                                                    multiple: true,
                                                                    prompt: Some(
                                                                        "Select PDF Files".into(),
                                                                    ),
                                                                },
                                                            )
                                                        })
                                                        .ok()?;

                                                    if let Ok(Some(paths)) =
                                                        receiver.await.ok()?
                                                    {
                                                        state
                                                            .update(cx, |state, cx| {
                                                                state.add_attachments(paths, cx);
                                                            })
                                                            .ok()?;
                                                    }
                                                    Some(())
                                                })
                                                .detach();
                                            },
                                        ),
                                )
                            })
                    }),
            )
        } else {
            None
        };

        div()
            .border_1()
            .px_3()
            .py_3()
            .rounded_2xl()
            .border_color(rgb(0xe5e7eb))
            .bg(cx.theme().secondary)
            .child(
                div()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .child(Input::new(&input_entity).appearance(false)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .when_some(attachment_popover, |d, popover| d.child(popover))
                            .child(div().flex_grow())
                            .child(model_popover)
                            .child(
                                // Send button
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(rgb(0xffa033))
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0xff8c1a)))
                                    .child("Send")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_event, _window, cx| {
                                            state_for_send.update(cx, |state, cx| {
                                                state.send_message(cx);
                                            });
                                        },
                                    ),
                            ),
                    )
                    .when(!attachments.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .p_2()
                                .mt_2()
                                .rounded_lg()
                                .children(
                                    attachments
                                        .iter()
                                        .enumerate()
                                        .map(|(index, path)| {
                                            render_file_chip(path, index, &self.state)
                                        }),
                                ),
                        )
                    }),
            )
    }
}
