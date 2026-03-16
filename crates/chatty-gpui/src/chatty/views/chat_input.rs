use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::popover::Popover;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};

use super::attachment_validation::{PDF_EXTENSION, is_image_extension, validate_attachment};
use crate::assets::CustomIcon;
use crate::chatty::services::pdf_thumbnail::render_pdf_thumbnail;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use std::collections::HashMap;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Slash command menu
// ---------------------------------------------------------------------------

/// A single entry in the slash-command picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub command: &'static str,
    pub description: &'static str,
    /// Text that is inserted into the input when the command is selected.
    pub insert_text: &'static str,
    /// When true the command is sent immediately on selection; when false
    /// the insert_text is placed into the input so the user can add args.
    pub execute_immediately: bool,
}

/// A skill loaded from the filesystem for display in the slash-command picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillEntry {
    /// Short directory name of the skill (e.g. `"fix-ci"`).
    pub name: String,
    /// Human-readable description extracted from the skill's frontmatter.
    pub description: String,
}

/// A combined item in the slash-command picker: either a built-in command or a
/// dynamic skill loaded from the filesystem.
#[derive(Clone, Debug, PartialEq)]
pub enum SlashMenuItem {
    Command(&'static SlashCommand),
    Skill(SkillEntry),
}

impl SlashMenuItem {
    /// The slash-prefixed display string shown in the menu (e.g. `/compact` or `/fix-ci`).
    pub fn display_command(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.command.to_string(),
            SlashMenuItem::Skill(skill) => format!("/{}", skill.name),
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &str {
        match self {
            SlashMenuItem::Command(cmd) => cmd.description,
            SlashMenuItem::Skill(skill) => &skill.description,
        }
    }

    /// Whether the item should be applied immediately (no arg input needed).
    pub fn execute_immediately(&self) -> bool {
        match self {
            SlashMenuItem::Command(cmd) => cmd.execute_immediately,
            // Skills are not execute-immediately — we insert a prompt the user
            // can review and optionally extend before pressing Enter.
            SlashMenuItem::Skill(_) => false,
        }
    }

    /// Text to insert into the input when this item is selected.
    pub fn insert_text(&self) -> String {
        match self {
            SlashMenuItem::Command(cmd) => cmd.insert_text.to_string(),
            SlashMenuItem::Skill(skill) => format!("Use the '{}' skill: ", skill.name),
        }
    }

    /// Returns true when this item represents a filesystem skill.
    pub fn is_skill(&self) -> bool {
        matches!(self, SlashMenuItem::Skill(_))
    }
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        command: "/add-dir",
        description: "Add a directory to allowed workspace access",
        insert_text: "/add-dir ",
        execute_immediately: false,
    },
    SlashCommand {
        command: "/agent",
        description: "Launch a sub-agent with a prompt",
        insert_text: "/agent ",
        execute_immediately: false,
    },
    SlashCommand {
        command: "/clear",
        description: "Clear conversation history",
        insert_text: "/clear",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/new",
        description: "Start a new conversation",
        insert_text: "/new",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/compact",
        description: "Summarize conversation history to reduce context",
        insert_text: "/compact",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/context",
        description: "Show context window usage",
        insert_text: "/context",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/copy",
        description: "Copy latest response to clipboard",
        insert_text: "/copy",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/cwd",
        description: "Show current working directory",
        insert_text: "/cwd",
        execute_immediately: true,
    },
    SlashCommand {
        command: "/cd",
        description: "Change working directory",
        insert_text: "/cd ",
        execute_immediately: false,
    },
];

/// Returns the built-in slash commands that match the current `input_text`.
/// The menu is active only when `input_text` starts with `/` and contains
/// no whitespace (once there is a space the user is typing arguments).
///
/// Use [`slash_menu_items_with_skills`] to also include filesystem skills.
pub fn slash_menu_items_for(input_text: &str) -> Vec<&'static SlashCommand> {
    let trimmed = input_text.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    // Once the user has typed a space (argument separator) close the menu.
    if trimmed.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let query = trimmed[1..].to_ascii_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|cmd| {
            query.is_empty()
                || cmd
                    .command
                    .trim_start_matches('/')
                    .to_ascii_lowercase()
                    .starts_with(&query)
        })
        .collect()
}

/// Returns combined slash-menu items: built-in commands first, then filesystem
/// skills — both filtered to match the current query in `input_text`.
pub fn slash_menu_items_with_skills(
    input_text: &str,
    skills: &[SkillEntry],
) -> Vec<SlashMenuItem> {
    let trimmed = input_text.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let query = trimmed[1..].to_ascii_lowercase();

    let mut items: Vec<SlashMenuItem> = SLASH_COMMANDS
        .iter()
        .filter(|cmd| {
            query.is_empty()
                || cmd
                    .command
                    .trim_start_matches('/')
                    .to_ascii_lowercase()
                    .starts_with(&query)
        })
        .map(|cmd| SlashMenuItem::Command(cmd))
        .collect();

    let skill_items = skills.iter().filter(|skill| {
        query.is_empty() || skill.name.to_ascii_lowercase().starts_with(&query)
    });
    items.extend(skill_items.map(|s| SlashMenuItem::Skill(s.clone())));

    items
}

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

/// State for the chat input component
/// Cache for PDF thumbnails: maps PDF path -> thumbnail path or error
type ThumbnailCache = Arc<RwLock<HashMap<PathBuf, Result<PathBuf, String>>>>;

pub struct ChatInputState {
    pub input: Entity<InputState>,
    attachments: Vec<PathBuf>,
    should_clear: bool,
    selected_model_id: Option<String>,
    available_models: Vec<(String, String)>, // (id, display_name)
    supports_images: bool,
    supports_pdf: bool,
    thumbnail_cache: ThumbnailCache,
    is_streaming: bool,
    /// Index of the highlighted item in the slash-command picker.
    slash_menu_selected: usize,
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
            last_slash_query: None,
            pending_slash_insert: None,
            working_dir: None,
            available_skills: Vec::new(),
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
                    if is_pdf(&path) {
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
    }

    // -----------------------------------------------------------------------
    // Slash-command menu helpers
    // -----------------------------------------------------------------------

    /// Whether the slash-command picker should be shown given the current input.
    pub fn is_slash_menu_open(&self, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).text().to_string();
        !slash_menu_items_with_skills(&text, &self.available_skills).is_empty()
    }

    /// Current highlighted index in the picker.
    pub fn slash_menu_selected(&self) -> usize {
        self.slash_menu_selected
    }

    /// Reset the selection to 0 **only** when the slash query text changes.
    ///
    /// This is called from the `InputEvent::Change` subscriber.  We deliberately
    /// ignore spurious Change events that don't alter the query (e.g. the
    /// newline that gpui-component appends to the buffer right before it fires
    /// `InputEvent::PressEnter` in an auto-grow input) so that the arrow-key
    /// selection is still respected when the user presses Enter.
    pub fn reset_slash_menu_selection_if_query_changed(&mut self, new_text: &str) {
        // Extract the raw query slice (text after '/', no leading slash).
        // If there is no leading '/' or the text already contains whitespace
        // (menu would be closed anyway), treat as no active query.
        let trimmed = new_text.trim();
        let query_raw: &str =
            if trimmed.starts_with('/') && !trimmed.chars().any(char::is_whitespace) {
                &trimmed[1..]
            } else {
                ""
            };

        // Compare without allocating; only convert to owned when storing.
        let changed = self
            .last_slash_query
            .as_deref()
            .map(|prev| !prev.eq_ignore_ascii_case(query_raw))
            .unwrap_or(true);

        if changed {
            self.slash_menu_selected = 0;
            self.last_slash_query = Some(query_raw.to_ascii_lowercase());
        }
    }

    /// Move selection up (wraps to last item).
    pub fn move_slash_menu_up(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        if self.slash_menu_selected == 0 {
            self.slash_menu_selected = num_items - 1;
        } else {
            self.slash_menu_selected -= 1;
        }
    }

    /// Move selection down (wraps to first item).
    pub fn move_slash_menu_down(&mut self, num_items: usize) {
        if num_items == 0 {
            return;
        }
        self.slash_menu_selected = (self.slash_menu_selected + 1) % num_items;
    }

    /// Apply the currently highlighted slash command or skill.
    ///
    /// * For immediate commands (no args needed) the command is emitted via
    ///   `ChatInputEvent::SlashCommandSelected` and the input is cleared.
    /// * For argument commands (and all skills) the `insert_text` is written
    ///   into the input on the next render frame via `pending_slash_insert`.
    pub fn apply_slash_command(&mut self, cx: &mut Context<Self>) {
        let input_text = self.input.read(cx).text().to_string();
        let items = slash_menu_items_with_skills(&input_text, &self.available_skills);
        if items.is_empty() {
            return;
        }
        let selected = self.slash_menu_selected.min(items.len().saturating_sub(1));
        let item = &items[selected];
        self.slash_menu_selected = 0;
        self.last_slash_query = None; // reset so next '/' starts fresh

        if item.execute_immediately() {
            // Only built-in commands reach here (skills are never immediate).
            if let SlashMenuItem::Command(cmd) = item {
                cx.emit(ChatInputEvent::SlashCommandSelected(
                    cmd.command.to_string(),
                ));
            }
            self.should_clear = true;
        } else {
            // Insert command text (with trailing space) so user can type args.
            self.pending_slash_insert = Some(item.insert_text());
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
        let _no_models = self.state.read(cx).available_models.is_empty();

        // --- Slash menu ---
        let input_text = input_entity.read(cx).text().to_string();
        let available_skills = self.state.read(cx).available_skills.clone();
        let menu_items = slash_menu_items_with_skills(&input_text, &available_skills);
        let slash_menu_selected = self.state.read(cx).slash_menu_selected();

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
                                        cx.emit(ChatInputEvent::ModelChanged(id_clone.clone()));
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

        // The outer wrapper uses flex-col so the slash menu appears above the input box.
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
                                                                    {
                                                                        if let Some(path) =
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
// Slash-command menu renderer
// ---------------------------------------------------------------------------

/// Renders the slash-command picker above the input.
///
/// Items can be built-in commands or filesystem skills; skills are shown with
/// a purple accent colour and a `[skill]` badge to distinguish them visually.
fn render_slash_menu(
    items: &[SlashMenuItem],
    selected: usize,
    state: &Entity<ChatInputState>,
    cx: &App,
) -> impl IntoElement {
    let theme_bg = cx.theme().background;
    let theme_border = cx.theme().border;
    let theme_secondary = cx.theme().secondary;

    div()
        .w_full()
        .flex()
        .flex_col()
        .bg(theme_bg)
        .border_1()
        .border_color(theme_border)
        .rounded_lg()
        .shadow_md()
        .p_1()
        .children(items.iter().enumerate().map(|(idx, item)| {
            let state_for_click = state.clone();
            let display_command = item.display_command();
            let description = item.description().to_string();
            let is_skill = item.is_skill();
            let is_selected = idx == selected.min(items.len().saturating_sub(1));

            // Skills use a purple accent; commands use the standard blue.
            let command_color = if is_skill {
                rgb(0x8b5cf6)
            } else {
                rgb(0x3b82f6)
            };

            div()
                .id(ElementId::Name(format!("slash-cmd-{}", display_command).into()))
                .px_3()
                .py_2()
                .rounded_sm()
                .cursor_pointer()
                .flex()
                .flex_row()
                .gap_3()
                .when(is_selected, |d| d.bg(theme_secondary))
                .hover(|style| style.bg(theme_secondary))
                // Highlight on hover to update selected index
                .on_mouse_move({
                    let state = state.clone();
                    move |_event, _window, cx| {
                        state.update(cx, |s, cx| {
                            if s.slash_menu_selected != idx {
                                s.slash_menu_selected = idx;
                                cx.notify();
                            }
                        });
                    }
                })
                .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                    state_for_click.update(cx, |s, cx| {
                        s.slash_menu_selected = idx;
                        s.apply_slash_command(cx);
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(command_color)
                        .child(display_command),
                )
                .when(is_skill, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8b5cf6))
                            .px_1()
                            .border_1()
                            .border_color(rgb(0x8b5cf6))
                            .rounded_sm()
                            .child("skill"),
                    )
                })
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x6b7280))
                        .child(description),
                )
        }))
        .child(
            // Help footer
            div()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(rgb(0x9ca3af))
                .child("↑↓ navigate  ·  Enter to apply  ·  Esc to dismiss"),
        )
}

#[cfg(test)]
#[path = "chat_input_test.rs"]
mod tests;
