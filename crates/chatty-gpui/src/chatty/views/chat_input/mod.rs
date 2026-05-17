//! Chat input field — the bottom composition area of the chat view.
//!
//! # What lives here
//!
//! - `ChatInputState` entity — text buffer, attachment list, selected
//!   model/provider, capabilities (image/PDF support), slash-command and
//!   skill (`@`-mention) popovers.
//! - `ChatInputEvent` — events emitted to `ChattyApp` / `ChatView`
//!   (send, model change, attachment added/removed, slash command, …).
//! - Keyboard handling, drag-and-drop, paste of images/files, autocomplete
//!   popovers, and the model picker.
//!
//! # What does NOT live here
//!
//! - Message rendering / scrolling — `chat_view.rs`.
//! - Slash-command dispatch — `chatty::controllers::app_controller::slash_commands`.
//! - Capability data — looked up from `ModelsModel` (chatty-core).
//!
//! Capability propagation flows `ModelsModel -> set_capabilities() ->
//! UI button visibility -> send-time filter` (see CLAUDE.md "Model
//! Capability Architecture").

mod at_mention;
mod render;
mod slash;

// Re-export the public surface for external callers and the unit-test
// module so the `chat_input::*` namespace is unchanged after the
// directory split.
#[cfg(test)]
pub use at_mention::{apply_at_to_input, at_menu_items_for, at_query_from};
#[allow(unused_imports)] // load_files_for_dir is part of the public API
pub use at_mention::load_files_for_dir;
pub use slash::{SkillEntry, slash_menu_items_with_skills};
#[allow(unused_imports)] // SlashCommand / SlashMenuItem are part of the public API
pub use slash::{SlashCommand, SlashMenuItem};
#[cfg(test)]
pub use slash::slash_menu_items_for;

use gpui::*;
use gpui_component::input::InputState;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, warn};

use super::attachment_validation::validate_attachment;
use crate::chatty::services::pdf_thumbnail::render_pdf_thumbnail;
use crate::settings::models::providers_store::ProviderType;
use std::collections::HashMap;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Events emitted by ChatInputState for entity-to-entity communication
#[derive(Clone, Debug)]
pub enum ChatInputEvent {
    Send {
        message: String,
        attachments: Vec<PathBuf>,
    },
    ModelChanged(String),
    Stop,
    /// A slash command that should be executed immediately (no args required).
    SlashCommandSelected(String),
    WorkingDirChanged(Option<PathBuf>),
}

impl EventEmitter<ChatInputEvent> for ChatInputState {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub provider_type: ProviderType,
}

impl ModelOption {
    pub fn new(id: String, name: String, provider_type: ProviderType) -> Self {
        Self {
            id,
            name,
            provider_type,
        }
    }
}

/// State for the chat input component
/// Cache for PDF thumbnails: maps PDF path -> thumbnail path or error
pub(super) type ThumbnailCache = Arc<RwLock<HashMap<PathBuf, Result<PathBuf, String>>>>;

pub struct ChatInputState {
    pub input: Entity<InputState>,
    attachments: Vec<PathBuf>,
    should_clear: bool,
    selected_model_id: Option<String>,
    available_models: Vec<ModelOption>,
    supports_images: bool,
    supports_pdf: bool,
    thumbnail_cache: ThumbnailCache,
    is_streaming: bool,
    /// Index of the highlighted item in the slash-command picker.
    slash_menu_selected: usize,
    /// Scroll state for the slash-command picker so keyboard navigation can
    /// keep the selected item visible.
    slash_menu_scroll_handle: ScrollHandle,
    /// The slash query that was in effect when `slash_menu_selected` was last
    /// reset.  Used to detect genuine query changes vs. spurious Change events
    /// (e.g. the newline that gpui-component appends before firing PressEnter).
    last_slash_query: Option<String>,
    /// When set, the value is written into the input on the next render frame
    /// (requires Window access, deferred from the subscription closure).
    pending_slash_insert: Option<String>,
    /// Per-conversation working directory override (None = use global workspace_dir setting)
    working_dir: Option<PathBuf>,
    /// Filesystem skills loaded from the workspace `.claude/skills/` and global skills
    /// directories.  Updated whenever the working directory changes.
    available_skills: Vec<SkillEntry>,
    /// Cached list of files for the `@` mention picker (loaded on first use).
    at_menu_files: Vec<String>,
    /// Index of the highlighted item in the `@` mention picker.
    at_menu_selected: usize,
    /// Scroll state for the `@` mention picker so keyboard navigation can keep
    /// the selected item visible.
    at_menu_scroll_handle: ScrollHandle,
    /// Last `@` query seen when `at_menu_selected` was reset (change detection).
    last_at_query: Option<String>,
    /// When set, this text is written into the input on the next render frame.
    pending_at_insert: Option<String>,
}

impl ChatInputState {
    pub fn new(input: Entity<InputState>) -> Self {
        Self {
            input,
            attachments: Vec::new(),
            should_clear: false,
            selected_model_id: None,
            thumbnail_cache: Arc::new(RwLock::new(HashMap::new())),
            available_models: Vec::new(),
            supports_images: false,
            supports_pdf: false,
            is_streaming: false,
            slash_menu_selected: 0,
            slash_menu_scroll_handle: ScrollHandle::new(),
            last_slash_query: None,
            pending_slash_insert: None,
            working_dir: None,
            available_skills: Vec::new(),
            at_menu_files: Vec::new(),
            at_menu_selected: 0,
            at_menu_scroll_handle: ScrollHandle::new(),
            last_at_query: None,
            pending_at_insert: None,
        }
    }

    /// Replace the cached list of filesystem skills and notify GPUI to re-render.
    pub fn set_available_skills(&mut self, skills: Vec<SkillEntry>, cx: &mut Context<Self>) {
        self.available_skills = skills;
        cx.notify();
    }

    /// Return the currently loaded skills (used for rendering the menu).
    pub fn available_skills(&self) -> &[SkillEntry] {
        &self.available_skills
    }

    /// Set available models for selection
    pub fn set_available_models(&mut self, models: Vec<ModelOption>, default_id: Option<String>) {
        self.available_models = models;

        if self.selected_model_id.is_none() {
            self.selected_model_id =
                default_id.or_else(|| self.available_models.first().map(|m| m.id.clone()));
        }
    }

    /// Get the available models list
    pub fn available_models(&self) -> &[ModelOption] {
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

    /// Set streaming state
    pub fn set_streaming(&mut self, streaming: bool, cx: &mut Context<Self>) {
        self.is_streaming = streaming;
        cx.notify();
    }

    /// Check if currently streaming
    pub fn is_streaming(&self) -> bool {
        self.is_streaming
    }

    /// Get the per-conversation working directory override currently shown in the input UI
    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.working_dir.as_ref()
    }

    /// Set the per-conversation working directory override and emit event
    pub fn set_working_dir(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        self.working_dir = dir.clone();
        // Invalidate the cached file list so it is reloaded from the new dir.
        self.at_menu_files.clear();
        cx.emit(ChatInputEvent::WorkingDirChanged(dir));
        cx.notify();
    }

    /// Set the working directory without emitting an event (for restoring state on conversation load)
    pub fn set_working_dir_silent(&mut self, dir: Option<PathBuf>) {
        self.working_dir = dir;
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
                    // Start thumbnail generation for PDFs immediately in background
                    if path
                        .extension()
                        .map(|ext| {
                            ext.to_string_lossy().to_lowercase()
                                == super::attachment_validation::PDF_EXTENSION
                        })
                        .unwrap_or(false)
                    {
                        self.start_thumbnail_generation_for_pdf(path.clone());
                    }
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

    /// Start background thumbnail generation for a PDF (called when attachment is added)
    fn start_thumbnail_generation_for_pdf(&self, pdf_path: PathBuf) {
        let cache = self.thumbnail_cache.clone();

        // Check if already cached or in progress
        if let Ok(guard) = cache.try_read()
            && guard.contains_key(&pdf_path)
        {
            return; // Already generating or cached
        }

        // Mark as in-progress immediately to prevent duplicate work
        if let Ok(mut cache_write) = cache.try_write() {
            cache_write
                .entry(pdf_path.clone())
                .or_insert_with(|| Err("Generating...".to_string()));
        }

        // Spawn background task on the tokio runtime
        let cache_for_task = cache.clone();
        let pdf_path_for_result = pdf_path.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                render_pdf_thumbnail(&pdf_path)
                    .map_err(|e| format!("Failed to generate thumbnail: {}", e))
            })
            .await;

            let thumbnail_result = match result {
                Ok(res) => res,
                Err(e) => Err(format!("Task error: {}", e)),
            };

            // Update cache with result
            if let Ok(mut cache_write) = cache_for_task.try_write() {
                cache_write.insert(pdf_path_for_result, thumbnail_result);
            }

            // Note: UI will be updated on next render when attachments are displayed
        });
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

        debug!("Emitting ChatInputEvent::Send");
        cx.emit(ChatInputEvent::Send {
            message: message.clone(),
            attachments: attachments.clone(),
        });

        self.should_clear = true;
        self.clear_attachments();
        debug!("Marked input for clearing");
    }

    /// Stop the current stream
    pub fn stop_stream(&mut self, cx: &mut Context<Self>) {
        debug!("stop_stream called, emitting ChatInputEvent::Stop");
        cx.emit(ChatInputEvent::Stop);
    }

    /// Mark the input for clearing on next render (without sending)
    pub fn mark_for_clear(&mut self) {
        self.should_clear = true;
    }

    /// Clear the input if needed
    pub fn clear_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_clear {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.should_clear = false;
        }
        // Apply a pending slash-command text insert (for commands that need arguments).
        // Use set_value("") + insert(text) instead of set_value(text) so that the
        // cursor lands at the END of the inserted text (set_value resets to offset 0
        // in multi-line/auto_grow mode).
        if let Some(text) = self.pending_slash_insert.take() {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx); // clear → cursor at offset 0
                input.insert(&text, window, cx); // insert and move cursor to end
            });
        }
        // Apply a pending @ mention insert.
        if let Some(text) = self.pending_at_insert.take() {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.insert(&text, window, cx);
            });
        }
    }


    /// Get the selected model ID
    pub fn selected_model_id(&self) -> Option<&String> {
        self.selected_model_id.as_ref()
    }

    /// Get display name for selected model
    pub fn get_selected_model_display_name(&self) -> String {
        self.selected_model()
            .map(|model| format!("{} · {}", model.name, model.provider_type.display_name()))
            .unwrap_or_else(|| {
                if self.available_models.is_empty() {
                    "No models".to_string()
                } else {
                    "Select Model".to_string()
                }
            })
    }

    pub fn selected_model(&self) -> Option<&ModelOption> {
        self.selected_model_id
            .as_ref()
            .and_then(|id| self.available_models.iter().find(|model| model.id == *id))
    }
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

#[cfg(test)]
#[path = "../chat_input_test.rs"]
mod tests;
