//! `RenderOnce` implementation for the `ChatInput` element.
//!
//! # What lives here
//!
//! - `impl RenderOnce for ChatInput` — the giant element tree for the
//!   composition area (text input + attachment chips + send/stop +
//!   model picker + slash/at popovers).
//! - `render_file_chip` — single-attachment thumbnail with remove button.
//! - Local helpers `is_image`, `is_pdf`, `provider_icon`.
//!
//! This is split out so the visual layout can be reviewed and modified
//! without scrolling past 1000 lines of state-management code. The
//! actual *behaviour* (events, mutations) all lives in `mod.rs` and
//! the slash/at submodules; this file is pure rendering.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::popover::Popover;
use gpui_component::scroll::ScrollableElement;
use gpui_component::tooltip::Tooltip;
use std::path::{Path, PathBuf};

use crate::assets::CustomIcon;
use crate::settings::models::providers_store::ProviderType;

use super::super::attachment_validation::{PDF_EXTENSION, is_image_extension};
use super::ThumbnailCache;
use super::at_mention::{at_menu_items_for, render_at_menu};
use super::slash::{render_slash_menu, slash_menu_items_with_skills};
use super::{ChatInput, ChatInputEvent, ChatInputState};
use crate::settings::models::execution_settings::ExecutionSettingsModel;

// ---------------------------------------------------------------------------
// Path / type helpers
// ---------------------------------------------------------------------------

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

fn provider_icon(provider_type: &ProviderType) -> CustomIcon {
    match provider_type {
        ProviderType::Ollama => CustomIcon::Ollama,
        ProviderType::OpenRouter => CustomIcon::OpenRouter,
        ProviderType::AzureOpenAI => CustomIcon::Azure,
    }
}

fn render_file_chip(
    path: &Path,
    index: usize,
    state: &Entity<ChatInputState>,
    thumbnail_cache: &ThumbnailCache,
) -> impl IntoElement {
    let state_clone = state.clone();

    // Determine display path based on file type
    let display_path = if is_image(path) {
        // Images can be displayed directly
        Some(path.to_path_buf())
    } else if is_pdf(path) {
        // For PDFs, check cache (generation started in add_attachments)
        // Use blocking read since we're not in a window context
        // Check the thumbnail cache (non-blocking)
        thumbnail_cache
            .try_read()
            .ok()
            .and_then(|guard| guard.get(path).and_then(|r| r.as_ref().ok()).cloned())
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
            // Show placeholder for PDFs (loading or no preview)
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
                    state_clone.update(cx, |state, _cx| {
                        state.remove_attachment(index);
                    });
                }),
        )
}

// ---------------------------------------------------------------------------
// RenderOnce impl
// ---------------------------------------------------------------------------

impl RenderOnce for ChatInput {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state_for_send = self.state.clone();
        let state_for_stop = self.state.clone();
        let state_for_model = self.state.clone();
        let state_for_image = self.state.clone();
        let state_for_pdf = self.state.clone();
        let state_for_dir = self.state.clone();
        let state_for_dir_reset = self.state.clone();
        let input_entity = self.state.read(cx).input.clone();

        // Read capabilities and attachments
        let supports_images = self.state.read(cx).supports_images;
        let supports_pdf = self.state.read(cx).supports_pdf;
        let show_attachment_button = supports_images || supports_pdf;
        let attachments = self.state.read(cx).get_attachments().to_vec();
        let is_streaming = self.state.read(cx).is_streaming();

        // Read thumbnail cache (for PDF previews)
        let thumbnail_cache = self.state.read(cx).thumbnail_cache.clone();

        // Working directory: per-chat override or global default
        let per_chat_working_dir = self.state.read(cx).working_dir.clone();
        let global_workspace_dir = cx
            .try_global::<ExecutionSettingsModel>()
            .and_then(|s| s.workspace_dir.clone())
            .map(PathBuf::from);
        let effective_working_dir = per_chat_working_dir.clone().or(global_workspace_dir);
        let has_working_dir_override = per_chat_working_dir.is_some();

        // Model display name
        let model_display = self.state.read(cx).get_selected_model_display_name();
        let selected_model = self.state.read(cx).selected_model().cloned();
        let _no_models = self.state.read(cx).available_models.is_empty();

        // --- Slash menu ---
        let input_text = input_entity.read(cx).text().to_string();
        let available_skills = self.state.read(cx).available_skills.clone();
        let menu_items = slash_menu_items_with_skills(&input_text, &available_skills);
        let slash_menu_selected = self.state.read(cx).slash_menu_selected();

        // --- @ mention menu ---
        let at_items: Vec<String> = {
            let state = self.state.read(cx);
            at_menu_items_for(&input_text, &state.at_menu_files)
                .into_iter()
                .cloned()
                .collect()
        };
        let at_menu_selected = self.state.read(cx).at_menu_selected();

        // Model dropdown button
        let model_button = if let Some(model) = selected_model {
            Button::new("model-select")
                .label(model_display.clone())
                .icon(Icon::new(provider_icon(&model.provider_type)).size_3())
        } else {
            Button::new("model-select").label(model_display.clone())
        };

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
                        d.child(
                            div()
                                .max_h(px(300.0))
                                .overflow_y_scrollbar()
                                .flex()
                                .flex_col()
                                .children(models.iter().map(|model| {
                                    let id_clone = model.id.clone();
                                    let provider_name =
                                        model.provider_type.display_name().to_string();
                                    let state_for_click = state.clone();
                                    let is_selected = selected_id.as_ref() == Some(&model.id);

                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .when(is_selected, |d| d.bg(cx.theme().secondary))
                                        .hover(|style| style.bg(cx.theme().secondary))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_3()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            Icon::new(provider_icon(
                                                                &model.provider_type,
                                                            ))
                                                            .size_3(),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .child(model.name.clone()),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(provider_name),
                                                ),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_event, _window, cx| {
                                                state_for_click.update(cx, |s, cx| {
                                                    s.selected_model_id = Some(id_clone.clone());
                                                    cx.emit(ChatInputEvent::ModelChanged(
                                                        id_clone.clone(),
                                                    ));
                                                    cx.notify();
                                                });
                                            },
                                        )
                                })),
                        )
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
                                                            cx.prompt_for_paths(PathPromptOptions {
                                                                files: true,
                                                                directories: false,
                                                                multiple: true,
                                                                prompt: Some(
                                                                    "Select Images".into(),
                                                                ),
                                                            })
                                                        })
                                                        .ok()?;

                                                    if let Ok(Some(paths)) = receiver.await.ok()? {
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
                                                            cx.prompt_for_paths(PathPromptOptions {
                                                                files: true,
                                                                directories: false,
                                                                multiple: true,
                                                                prompt: Some(
                                                                    "Select PDF Files".into(),
                                                                ),
                                                            })
                                                        })
                                                        .ok()?;

                                                    if let Ok(Some(paths)) = receiver.await.ok()? {
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

        // The outer wrapper uses flex-col so the slash/@ menus appear above the input box.
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            // Slash-command menu (visible when input starts with "/")
            .when(!menu_items.is_empty(), |d| {
                let state_for_menu = self.state.clone();
                d.child(render_slash_menu(
                    &menu_items,
                    slash_menu_selected,
                    &state_for_menu,
                    &self.state.read(cx).slash_menu_scroll_handle,
                    cx,
                ))
            })
            // @ mention menu (visible when input ends with "@<query>")
            .when(!at_items.is_empty(), |d| {
                let state_for_at = self.state.clone();
                d.child(render_at_menu(
                    &at_items,
                    at_menu_selected,
                    &state_for_at,
                    &self.state.read(cx).at_menu_scroll_handle,
                    cx,
                ))
            })
            // Main input box
            .child(
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
                                    .when_some(effective_working_dir, |d, dir| {
                                        // Compute display name: last path component or full path
                                        let dir_name = dir
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_else(|| dir.to_string_lossy().to_string());
                                        let full_path = dir.to_string_lossy().to_string();
                                        let full_path_for_tooltip = full_path.clone();
                                        d.child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .id("working-dir-selector")
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .px_2()
                                                        .py_1()
                                                        .rounded_sm()
                                                        .cursor_pointer()
                                                        .text_xs()
                                                        .text_color(rgb(0x6b7280))
                                                        .hover(|s| s.bg(rgb(0xe5e7eb)))
                                                        .tooltip(move |window, cx| {
                                                            Tooltip::new(
                                                                full_path_for_tooltip.clone(),
                                                            )
                                                            .build(window, cx)
                                                        })
                                                        .child(
                                                            Icon::new(CustomIcon::FolderOpen)
                                                                .size_3(),
                                                        )
                                                        .child(dir_name)
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            move |_event, _window, cx| {
                                                                let state =
                                                                    state_for_dir.clone();
                                                                cx.spawn(async move |cx| {
                                                                    let receiver = cx
                                                                        .update(|cx| {
                                                                            cx.prompt_for_paths(
                                                                                PathPromptOptions {
                                                                                    files: false,
                                                                                    directories:
                                                                                        true,
                                                                                    multiple: false,
                                                                                    prompt: Some(
                                                                                        "Select Working Directory".into(),
                                                                                    ),
                                                                                },
                                                                            )
                                                                        })
                                                                        .ok()?;

                                                                    if let Ok(Some(paths)) =
                                                                        receiver.await.ok()?
                                                                        && let Some(path) =
                                                                            paths.into_iter().next()
                                                                        {
                                                                            state
                                                                                .update(
                                                                                    cx,
                                                                                    |state, cx| {
                                                                                        state.set_working_dir(
                                                                                            Some(
                                                                                                path,
                                                                                            ),
                                                                                            cx,
                                                                                        );
                                                                                    },
                                                                                )
                                                                                .ok()?;
                                                                        }
                                                                    Some(())
                                                                })
                                                                .detach();
                                                            },
                                                        ),
                                                )
                                                .when(has_working_dir_override, |d| {
                                                    d.child(
                                                        div()
                                                            .id("working-dir-reset")
                                                            .px_1()
                                                            .py_1()
                                                            .rounded_sm()
                                                            .cursor_pointer()
                                                            .text_xs()
                                                            .text_color(rgb(0x9ca3af))
                                                            .hover(|s| s.bg(rgb(0xe5e7eb)))
                                                            .tooltip(|window, cx| {
                                                                Tooltip::new(
                                                                    "Reset to global working directory",
                                                                )
                                                                .build(window, cx)
                                                            })
                                                            .child("×")
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                move |_event, _window, cx| {
                                                                    state_for_dir_reset
                                                                        .update(
                                                                            cx,
                                                                            |state, cx| {
                                                                                state.set_working_dir(
                                                                                    None,
                                                                                    cx,
                                                                                );
                                                                            },
                                                                        );
                                                                },
                                                            ),
                                                    )
                                                }),
                                        )
                                    })
                                    .child(div().flex_grow())
                                    .child(model_popover)
                                    .child(
                                        // Send/Stop button (conditional based on streaming state)
                                        div()
                                            .px_3()
                                            .py_1()
                                            .rounded_sm()
                                            .text_color(rgb(0xffffff))
                                            .cursor_pointer()
                                            .when(is_streaming, |div| {
                                                // Stop button when streaming
                                                div.bg(rgb(0xff4444))
                                                    .hover(|style| style.bg(rgb(0xff2222)))
                                                    .child("Stop")
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        move |_event, _window, cx| {
                                                            state_for_stop.update(
                                                                cx,
                                                                |state, cx| {
                                                                    state.stop_stream(cx);
                                                                },
                                                            );
                                                        },
                                                    )
                                            })
                                            .when(!is_streaming, |div| {
                                                // Send button when not streaming
                                                div.bg(rgb(0xffa033))
                                                    .hover(|style| style.bg(rgb(0xff8c1a)))
                                                    .child("Send")
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        move |_event, _window, cx| {
                                                            state_for_send.update(
                                                                cx,
                                                                |state, cx| {
                                                                    state.send_message(cx);
                                                                },
                                                            );
                                                        },
                                                    )
                                            }),
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
                                        .children(attachments.iter().enumerate().map(
                                            |(index, path)| {
                                                render_file_chip(
                                                    path,
                                                    index,
                                                    &self.state,
                                                    &thumbnail_cache,
                                                )
                                            },
                                        )),
                                )
                            }),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------
