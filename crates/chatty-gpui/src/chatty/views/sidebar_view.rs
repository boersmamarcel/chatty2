use std::path::PathBuf;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, Collapsible, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{InputEvent, InputState},
    v_flex,
};

use super::conversation_item::ConversationItem;
use super::sidebar_file_tree::render_sidebar_file_tree;
use super::transcript::file_tree::{FileOp, FileTree, PendingEdit, SelectGesture};
use crate::assets::CustomIcon;
use crate::settings::models::execution_settings::ExecutionSettingsModel;
use crate::settings::models::general_model::SidebarMode;

/// Events emitted by SidebarView for entity-to-entity communication
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    NewChat,
    OpenSettings,
    SelectConversation(String),
    DeleteConversation(String),
    ExportConversation(String),
    /// Take this conversation online, or bring it back (AGE-298). Which of the
    /// two is meant follows the conversation's current mode, which the
    /// controller reads — the sidebar only says which conversation.
    MoveConversation(String),
    ToggleCollapsed(bool),
    LoadMore,
    /// A file was activated in Files mode (AGE-480): open it as an artifact
    /// tab, the way a tool card's "open" click does. The second field is the
    /// tree's root, carried along so the handler does not have to
    /// recompute it (and cannot race a conversation switch in between).
    OpenFile(PathBuf, PathBuf),
    /// The tree just changed something on disk (create/rename/delete); the
    /// second field is the tree's root, so the handler can rebuild a
    /// workspace-relative display path. The artifact panel reacts to keep
    /// its own open tabs and buffers in step (AGE-476 behavior, unchanged).
    FileOp(FileOp, PathBuf),
}

impl EventEmitter<SidebarEvent> for SidebarView {}

/// Sidebar view showing conversations, or (AGE-480) the active workspace's
/// file tree.
pub struct SidebarView {
    conversations: Vec<(String, String, Option<f64>)>, // (id, title, cost)
    active_conversation_id: Option<String>,
    is_collapsed: bool,
    // OPTIMIZATION: Pagination for sidebar
    visible_limit: usize, // How many conversations to show (starts at 20)
    total_count: usize,   // Total available conversations
    /// Chats or Files (AGE-480); persisted globally like the theme.
    mode: SidebarMode,
    /// The workspace explorer tree, `Some` once a root has been resolved.
    /// Built lazily the first time Files mode is shown, so a session that
    /// never opens it never lists a directory.
    explorer: Option<FileTree>,
    /// The inline name input the tree shows for a create/rename; one entity
    /// reused for every edit (AGE-476).
    explorer_input: Entity<InputState>,
    explorer_scroll: UniformListScrollHandle,
    /// Focus `explorer_input` on the next render: an edit begun from a
    /// context-menu item is focused after the menu's own dismissal has
    /// restored focus, or the menu wins.
    explorer_focus_pending: bool,
}

impl SidebarView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let explorer_input = cx.new(|cx| InputState::new(window, cx).placeholder("name"));
        cx.subscribe(
            &explorer_input,
            |this: &mut Self, input, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let name = input.read(cx).value().to_string();
                    let _ = this.explorer_commit_edit(name, cx);
                }
                // Clicking away commits a typed name and drops an empty or
                // rejected one, the way an IDE's inline rename behaves. A
                // blur from before the render that focuses the input (the
                // context menu closing) is not the user leaving.
                InputEvent::Blur => {
                    if this.explorer_focus_pending {
                        return;
                    }
                    let name = input.read(cx).value().to_string();
                    if name.trim().is_empty() || !this.explorer_commit_edit(name, cx) {
                        this.explorer_cancel_edit(cx);
                    }
                }
                _ => {}
            },
        )
        .detach();

        Self {
            conversations: Vec::new(),
            active_conversation_id: None,
            is_collapsed: false,
            visible_limit: 20, // Start with 20 conversations
            total_count: 0,
            mode: SidebarMode::default(),
            explorer: None,
            explorer_input,
            explorer_scroll: UniformListScrollHandle::default(),
            explorer_focus_pending: false,
        }
    }

    /// Set conversations to display
    pub fn set_conversations(
        &mut self,
        conversations: Vec<(String, String, Option<f64>)>,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(
            count = conversations.len(),
            "SidebarView: set_conversations called with {} conversations",
            conversations.len()
        );
        for (id, title, cost) in &conversations {
            tracing::debug!(id = %id, title = %title, cost = ?cost, "  - Conversation");
        }
        self.conversations = conversations;
        cx.notify();
    }

    /// Set the active conversation
    pub fn set_active_conversation(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.active_conversation_id = id;
        cx.notify();
    }

    /// Toggle the collapsed state of the sidebar
    pub fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.is_collapsed = !self.is_collapsed;
        cx.emit(SidebarEvent::ToggleCollapsed(self.is_collapsed));
        cx.notify();
    }

    /// Get the current collapsed state
    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    /// Get the current visible limit for pagination
    pub fn visible_limit(&self) -> usize {
        self.visible_limit
    }

    /// Set the total count of conversations
    pub fn set_total_count(&mut self, count: usize) {
        self.total_count = count;
    }

    /// Load more conversations (increase visible limit by 20)
    /// OPTIMIZATION: Allows progressive loading of conversation history
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        self.visible_limit += 20; // Load 20 more
        cx.notify();
    }

    // ----- Chats/Files mode (AGE-480) -----

    pub fn is_files_mode(&self) -> bool {
        self.mode == SidebarMode::Files
    }

    /// Switch mode and persist it — the footer toggle's handler.
    pub fn set_mode(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        self.apply_mode(mode, cx);
        crate::settings::controllers::general_settings_controller::update_sidebar_mode(cx, mode);
    }

    /// The titlebar artifact picker's "Files" entry (AGE-480): expand the
    /// sidebar if it is collapsed, and switch it to Files, so the tree is
    /// reachable no matter where the user starts from.
    pub fn show_files_mode(&mut self, cx: &mut Context<Self>) {
        if self.is_collapsed {
            self.is_collapsed = false;
            cx.emit(SidebarEvent::ToggleCollapsed(false));
        }
        self.set_mode(SidebarMode::Files, cx);
    }

    /// Apply the mode loaded from disk once general settings finish loading
    /// (main.rs runs this from the same callback that restores the theme).
    /// No re-save: this *is* the saved value.
    pub fn apply_persisted_mode(&mut self, cx: &mut Context<Self>) {
        let mode = cx
            .global::<crate::settings::models::GeneralSettingsModel>()
            .sidebar_mode;
        self.apply_mode(mode, cx);
    }

    fn apply_mode(&mut self, mode: SidebarMode, cx: &mut Context<Self>) {
        self.mode = mode;
        if mode == SidebarMode::Files {
            self.ensure_explorer_rooted(cx);
        }
        cx.notify();
    }

    /// Re-root the tree for the conversation that just became active
    /// (AGE-480): a conversation without its own working directory shows
    /// the same cwd fallback the explorer always has. A no-op while Chats
    /// mode is showing — the root is resolved lazily when Files is next
    /// shown, via [`Self::ensure_explorer_rooted`].
    pub fn reroot_for_conversation(
        &mut self,
        workspace_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.mode != SidebarMode::Files {
            return;
        }
        let root = effective_workspace_root(workspace_dir, cx);
        if self
            .explorer
            .as_ref()
            .is_none_or(|tree| tree.root() != root)
        {
            self.explorer = Some(FileTree::new(root));
            cx.notify();
        }
    }

    fn ensure_explorer_rooted(&mut self, cx: &App) {
        let root = active_workspace_root(cx);
        if self
            .explorer
            .as_ref()
            .is_none_or(|tree| tree.root() != root)
        {
            self.explorer = Some(FileTree::new(root));
        }
    }

    // ----- Workspace explorer (moved from the artifact panel, AGE-480;
    // originally AGE-476/478) -----

    /// A click on a tree row. A plain click selects that row alone and
    /// activates it (folders toggle, files open); Ctrl/⌘ and Shift only
    /// change the selection (AGE-476 multi-select).
    pub(super) fn explorer_click(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        gesture: SelectGesture,
        cx: &mut Context<Self>,
    ) {
        match gesture {
            SelectGesture::Single => self.explorer_activate(path, is_dir, cx),
            SelectGesture::Toggle | SelectGesture::Range => {
                if let Some(tree) = self.explorer.as_mut() {
                    tree.click_select(path, gesture);
                    cx.notify();
                }
            }
        }
    }

    /// A plain click on a tree row: folders toggle, files ask the panel to
    /// open them (the panel owns tabs, buffers and presentation, not the
    /// sidebar).
    pub(super) fn explorer_activate(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        tree.select(Some(path.clone()));
        if is_dir {
            tree.toggle(&path);
            cx.notify();
            return;
        }
        let root = tree.root().to_path_buf();
        cx.emit(SidebarEvent::OpenFile(path, root));
        cx.notify();
    }

    /// Drop of dragged entries onto `dest` (AGE-476 drag-to-move): the tree
    /// moves what can move and emits a `FileOp` per move so the artifact
    /// panel can keep any open tab in step.
    pub(super) fn explorer_drop(
        &mut self,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        let root = tree.root().to_path_buf();
        let ops = tree.move_entries(&paths, &dest);
        for op in ops {
            cx.emit(SidebarEvent::FileOp(op, root.clone()));
        }
        cx.notify();
    }

    pub(super) fn explorer_begin_edit(
        &mut self,
        edit: PendingEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        let text = edit.initial_text();
        tree.begin_edit(edit);
        self.explorer_input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        // Focused from the next render, once the row exists.
        self.explorer_focus_pending = true;
        cx.notify();
    }

    pub(super) fn explorer_cancel_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = self.explorer.as_mut()
            && tree.pending().is_some()
        {
            tree.cancel_edit();
            cx.notify();
        }
    }

    /// `false` when the name was refused; the tree then carries the reason.
    fn explorer_commit_edit(&mut self, name: String, cx: &mut Context<Self>) -> bool {
        let Some(tree) = self.explorer.as_mut() else {
            return true;
        };
        if tree.pending().is_none() {
            return true;
        }
        let Some(op) = tree.commit_edit(&name) else {
            // The edit stays open with the tree's error under it.
            cx.notify();
            return false;
        };
        let root = tree.root().to_path_buf();
        cx.emit(SidebarEvent::FileOp(op, root));
        cx.notify();
        true
    }

    /// Ask before deleting one entry or a multi-selection.
    pub(super) fn explorer_confirm_delete(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let what = delete_prompt(&paths);
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let paths = paths.clone();
            dialog
                .confirm()
                .title("Delete")
                .child(div().px_4().py_2().text_sm().child(what.clone()))
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(gpui_component::button::ButtonVariant::Danger),
                )
                .on_ok(move |_, _, cx| {
                    let paths = paths.clone();
                    entity.update(cx, |this, cx| this.explorer_delete(paths, cx));
                    true
                })
        });
    }

    fn explorer_delete(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(tree) = self.explorer.as_mut() else {
            return;
        };
        let root = tree.root().to_path_buf();
        let ops = tree.delete_many(&paths);
        for op in ops {
            cx.emit(SidebarEvent::FileOp(op, root.clone()));
        }
        cx.notify();
    }
}

/// Where the tree (and Cmd+P) root: the active conversation's own working
/// directory, else the global workspace setting, else the process cwd —
/// the same fallback order the filesystem tools use for a relative path.
pub(crate) fn active_workspace_root(cx: &App) -> PathBuf {
    let conversation_dir = cx
        .try_global::<chatty_core::models::ConversationsStore>()
        .and_then(|store| {
            store
                .active_id()
                .and_then(|id| store.get_conversation(id))
                .and_then(|conv| conv.working_dir().cloned())
        });
    effective_workspace_root(conversation_dir, cx)
}

/// [`active_workspace_root`], given a workspace dir already in hand (the
/// conversation-switch hook already reads it off the `Conversation` for its
/// own purposes, so it passes it in rather than this looking it up again).
fn effective_workspace_root(workspace_dir: Option<PathBuf>, cx: &App) -> PathBuf {
    workspace_dir
        .filter(|dir| !dir.as_os_str().is_empty())
        .or_else(|| {
            cx.try_global::<ExecutionSettingsModel>()
                .and_then(|settings| settings.workspace_dir.clone())
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// A human confirmation line for deleting one entry or a multi-selection
/// (AGE-476/478, moved from the artifact panel with the rest of the
/// explorer in AGE-480).
fn delete_prompt(paths: &[PathBuf]) -> String {
    let name = |path: &PathBuf| {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    };
    match paths {
        [one] if one.is_dir() => format!("Delete the folder '{}' and everything in it?", name(one)),
        [one] => format!("Delete '{}'?", name(one)),
        many => {
            const SHOWN: usize = 5;
            let mut listed: Vec<String> = many.iter().take(SHOWN).map(name).collect();
            if many.len() > SHOWN {
                listed.push(format!("… and {} more", many.len() - SHOWN));
            }
            format!(
                "Delete {} items? Folders go with everything in them. {}",
                many.len(),
                listed.join(", ")
            )
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::debug!(
            count = self.conversations.len(),
            "SidebarView: render called with {} conversations",
            self.conversations.len()
        );

        let sidebar_entity = cx.entity().clone();
        let active_id = self.active_conversation_id.clone();
        // Read once per render rather than per row: the store answers this
        // from the metadata layer, so no conversation is loaded to badge it.
        let hosted_ids: std::collections::HashSet<String> = cx
            .try_global::<crate::chatty::models::ConversationsStore>()
            .map(|store| {
                self.conversations
                    .iter()
                    .filter(|(id, _, _)| store.is_hosted(id))
                    .map(|(id, _, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        // AGE-308: the move between local and hosted is developer-only until
        // online mode is account-scoped, so the menu simply does not offer it.
        let move_enabled = crate::chatty::controllers::app_controller::move_ui_enabled(cx);

        let width = if self.is_collapsed { px(0.) } else { px(255.) };
        let files_mode = self.is_files_mode();

        // The explorer follows the disk on a timer (AGE-476); a `true` means
        // a listing changed, which this render already reflects.
        if files_mode && let Some(tree) = self.explorer.as_mut() {
            tree.sync(false);
            if self.explorer_focus_pending {
                self.explorer_focus_pending = false;
                if tree.pending().is_some() {
                    self.explorer_input
                        .update(cx, |input, cx| input.focus(window, cx));
                }
            }
        }

        v_flex()
            .id("sidebar")
            .w(width)
            .flex_shrink_0()
            .h_full()
            .overflow_hidden()
            .relative()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_color(cx.theme().sidebar_border)
            .when(!self.is_collapsed, |this| this.border_r_1())
            .when(self.is_collapsed, |this| this.gap_2())
            .when(!self.is_collapsed && !files_mode, |this| {
                this.child(
                    // Header: New Chat button
                    h_flex()
                        .id("header")
                        .pt_3()
                        .px_3()
                        .gap_2()
                        .when(self.is_collapsed, |this| this.pt_2().px_2())
                        // Add extra top padding on macOS for traffic light buttons
                        .when(cfg!(target_os = "macos"), |this| this.pt(px(40.0)))
                        .child(
                            Button::new("new-chat")
                                .primary()
                                .label(if self.is_collapsed { "+" } else { "New Chat" })
                                .small()
                                .rounded(px(999.))
                                .w_full()
                                .on_click({
                                    let entity = sidebar_entity.clone();
                                    move |_event, _window, cx| {
                                        entity.update(cx, |_, cx| {
                                            cx.emit(SidebarEvent::NewChat);
                                        });
                                    }
                                }),
                        ),
                )
            })
            .when(!self.is_collapsed && files_mode, |this| {
                // The tree draws its own header (the root folder name); the
                // traffic-light padding still applies on macOS.
                this.when(cfg!(target_os = "macos"), |this| this.pt(px(32.0)))
            })
            .when(!self.is_collapsed && !files_mode, |this| {
                this.child(
                    // Content: Conversation list
                    v_flex()
                        .id("content")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(
                            v_flex()
                                .id("inner")
                                .px_3()
                                .gap_y_1()
                                .when(self.is_collapsed, |this| this.p_2())
                                .children(
                                    self.conversations
                                        .iter()
                                        .enumerate()
                                        .map(|(ix, (id, title, cost))| {
                                            let is_active = active_id.as_ref() == Some(id);

                                            div()
                                                .id(ElementId::Name(id.clone().into()))
                                                .child(
                                                    ConversationItem::new(
                                                        id.clone(),
                                                        title.clone(),
                                                    )
                                                    .active(is_active)
                                                    .collapsed(self.is_collapsed)
                                                    .cost(*cost)
                                                    .on_click({
                                                        let entity = sidebar_entity.clone();
                                                        let id = id.clone();
                                                        move |_conv_id, cx| {
                                                            entity.update(cx, |_, cx| {
                                                                cx.emit(SidebarEvent::SelectConversation(id.clone()));
                                                            });
                                                        }
                                                    })
                                                    .on_delete({
                                                        let entity = sidebar_entity.clone();
                                                        let id = id.clone();
                                                        move |_conv_id, cx| {
                                                            entity.update(cx, |_, cx| {
                                                                cx.emit(SidebarEvent::DeleteConversation(id.clone()));
                                                            });
                                                        }
                                                    })
                                                    .on_export({
                                                        let entity = sidebar_entity.clone();
                                                        let id = id.clone();
                                                        move |_conv_id, cx| {
                                                            entity.update(cx, |_, cx| {
                                                                cx.emit(SidebarEvent::ExportConversation(id.clone()));
                                                            });
                                                        }
                                                    })
                                                    .hosted(hosted_ids.contains(id))
                                                    .when(move_enabled, |item| {
                                                        item.on_move({
                                                            let entity = sidebar_entity.clone();
                                                            let id = id.clone();
                                                            move |_conv_id, cx| {
                                                                entity.update(cx, |_, cx| {
                                                                    cx.emit(SidebarEvent::MoveConversation(id.clone()));
                                                                });
                                                            }
                                                        })
                                                    }),
                                                )
                                                .when(ix == 0, |this| this.mt_3())
                                                .when(
                                                    ix == self
                                                        .conversations
                                                        .len()
                                                        .saturating_sub(1),
                                                    |this| this.mb_3(),
                                                )
                                        })
                                        .collect::<Vec<_>>(),
                                )
                                // OPTIMIZATION: "Load More" button for pagination
                                .when(self.conversations.len() < self.total_count, |this| {
                                    this.child(
                                        div().px_3().py_2().child(
                                            Button::new("load-more-conversations")
                                                .label(format!(
                                                    "Load 20 more... ({}/{})",
                                                    self.conversations.len(),
                                                    self.total_count
                                                ))
                                                .small()
                                                .w_full()
                                                .on_click({
                                                    let entity = sidebar_entity.clone();
                                                    move |_event, _window, cx| {
                                                        entity.update(cx, |sidebar, cx| {
                                                            sidebar.load_more(cx);
                                                            cx.emit(SidebarEvent::LoadMore);
                                                        });
                                                    }
                                                }),
                                        ),
                                    )
                                }),
                        ),
                )
            })
            .when(!self.is_collapsed && files_mode, |this| {
                this.child(match self.explorer.as_ref() {
                    Some(tree) => render_sidebar_file_tree(
                        tree,
                        &self.explorer_input,
                        self.explorer_scroll.clone(),
                        sidebar_entity.clone(),
                        cx,
                    ),
                    None => div().flex_1().min_h_0().into_any_element(),
                })
            })
            .when(!self.is_collapsed, |this| {
                this.child(
                    // Footer: mode toggle + Settings button
                    h_flex()
                        .id("footer")
                        .pb_3()
                        .px_3()
                        .gap_2()
                        .when(self.is_collapsed, |this| this.pt_2().px_2())
                        .child(
                            // Chats | Files toggle (AGE-480): two icon buttons
                            // so the whole control collapses with the sidebar.
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("sidebar-mode-chats")
                                        .ghost()
                                        .small()
                                        .selected(!files_mode)
                                        .icon(Icon::new(CustomIcon::MessageSquare))
                                        .tooltip("Chats")
                                        .on_click({
                                            let entity = sidebar_entity.clone();
                                            move |_event, _window, cx| {
                                                entity.update(cx, |sidebar, cx| {
                                                    sidebar.set_mode(SidebarMode::Chats, cx);
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("sidebar-mode-files")
                                        .ghost()
                                        .small()
                                        .selected(files_mode)
                                        .icon(Icon::new(IconName::FolderClosed))
                                        .tooltip("Files")
                                        .on_click({
                                            let entity = sidebar_entity.clone();
                                            move |_event, _window, cx| {
                                                entity.update(cx, |sidebar, cx| {
                                                    sidebar.set_mode(SidebarMode::Files, cx);
                                                });
                                            }
                                        }),
                                ),
                        )
                        .child(
                            Button::new("settings")
                                .icon(Icon::new(IconName::Settings))
                                .label(if self.is_collapsed { "" } else { "Settings" })
                                .small()
                                .w_full()
                                .on_click({
                                    let entity = sidebar_entity.clone();
                                    move |_event, _window, cx| {
                                        entity.update(cx, |_, cx| {
                                            cx.emit(SidebarEvent::OpenSettings);
                                        });
                                    }
                                }),
                        ),
                )
            })
    }
}
