//! The artifact panel's explorer column (AGE-476): renders a [`FileTree`]
//! as an IDE-style tree with a header (new file, new folder, refresh), a
//! right-click menu per row, and an inline name input for create/rename.
//!
//! Every action routes back into [`ArtifactView`], which owns the tree; this
//! file only draws it.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable, h_flex, v_flex};

use super::artifact_view::ArtifactView;
use super::file_tree::{FileTree, PendingEdit, RowKind, SelectGesture, TreeRow};
use crate::assets::CustomIcon;

pub const EXPLORER_WIDTH: f32 = 220.0;
const ROW_HEIGHT: f32 = 24.0;
const INDENT: f32 = 12.0;

/// What a tree drag carries (AGE-476 drag-to-move): the whole selection
/// when a selected row is picked up, else just that row. Dropping on a
/// folder row moves into it, on a file row into that file's folder, on the
/// empty part of the list into the root.
#[derive(Clone, Debug)]
pub struct DraggedEntries {
    pub paths: Vec<PathBuf>,
}

/// The label that follows the pointer during a drag.
struct DragPreview {
    label: SharedString,
    folder: bool,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .shadow_md()
            .text_xs()
            .child(
                Icon::new(if self.folder {
                    IconName::FolderClosed
                } else {
                    IconName::File
                })
                .size_3()
                .text_color(cx.theme().muted_foreground),
            )
            .child(self.label.clone())
    }
}

fn drag_preview(paths: &[PathBuf]) -> DragPreview {
    match paths {
        [one] => DragPreview {
            label: one
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| one.display().to_string())
                .into(),
            folder: one.is_dir(),
        },
        many => DragPreview {
            label: format!("{} items", many.len()).into(),
            folder: false,
        },
    }
}

/// Which selection gesture the click's modifiers ask for.
fn gesture_for(modifiers: &Modifiers) -> SelectGesture {
    if modifiers.shift {
        SelectGesture::Range
    } else if modifiers.secondary() {
        SelectGesture::Toggle
    } else {
        SelectGesture::Single
    }
}

pub fn render_file_explorer(
    tree: &FileTree,
    name_input: &Entity<InputState>,
    scroll: UniformListScrollHandle,
    entity: Entity<ArtifactView>,
    cx: &App,
) -> AnyElement {
    let rows: Rc<Vec<TreeRow>> = Rc::new(tree.rows());
    let selection: Rc<Vec<PathBuf>> = Rc::new(tree.selection().to_vec());
    let root = tree.root().to_path_buf();
    let root_name = tree
        .root()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| tree.root().display().to_string());
    let root_path = tree.root().display().to_string();
    let error = tree.error().map(str::to_string);
    let name_input = name_input.clone();

    let header = h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .h(px(28.))
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .id("explorer-root")
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_ellipsis()
                .overflow_hidden()
                .whitespace_nowrap()
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(root_path.clone()).build(window, cx)
                })
                .child(root_name),
        )
        .child(
            Button::new("explorer-new-file")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Plus).size_3())
                .tooltip("New file")
                .on_click({
                    let entity = entity.clone();
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.explorer_begin_new(false, window, cx));
                    }
                }),
        )
        .child(
            Button::new("explorer-new-folder")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::FolderClosed).size_3())
                .tooltip("New folder")
                .on_click({
                    let entity = entity.clone();
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.explorer_begin_new(true, window, cx));
                    }
                }),
        )
        .child(
            Button::new("explorer-refresh")
                .ghost()
                .xsmall()
                .icon(Icon::new(CustomIcon::Refresh).size_3())
                .tooltip("Refresh")
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| this.explorer_refresh(cx));
                    }
                }),
        );

    let list = uniform_list("explorer-rows", rows.len(), {
        let rows = rows.clone();
        let entity = entity.clone();
        move |range, _window, cx| {
            range
                .map(|ix| render_row(ix, &rows[ix], &selection, &name_input, &entity, cx))
                .collect()
        }
    })
    .track_scroll(scroll)
    .flex_1()
    .min_h_0()
    .w_full();

    v_flex()
        .id("artifact-explorer")
        .w(px(EXPLORER_WIDTH))
        .h_full()
        .min_h_0()
        .flex_none()
        .border_r_1()
        .border_color(cx.theme().border)
        .child(header)
        .child(
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .py_1()
                // A drop past the last row lands in the root; a drop on a
                // row is taken by the row first and does not reach here.
                .on_drop::<DraggedEntries>({
                    let entity = entity.clone();
                    move |dragged, _, cx| {
                        let paths = dragged.paths.clone();
                        let dest = root.clone();
                        entity.update(cx, |this, cx| this.explorer_drop(paths, dest, cx));
                    }
                })
                .when(rows.is_empty(), |this| {
                    this.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Empty folder"),
                    )
                })
                .child(list),
        )
        .when_some(error, |this, message| {
            this.child(
                div()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(message),
            )
        })
        .into_any_element()
}

fn render_row(
    ix: usize,
    row: &TreeRow,
    selection: &Rc<Vec<PathBuf>>,
    name_input: &Entity<InputState>,
    entity: &Entity<ArtifactView>,
    cx: &App,
) -> AnyElement {
    let indent = px(8.0 + INDENT * row.depth as f32);
    match &row.kind {
        RowKind::Editor(edit) => {
            let is_dir_edit = matches!(edit, PendingEdit::NewFolder { .. })
                || matches!(edit, PendingEdit::Rename { path } if path.is_dir());
            h_flex()
                .id(("explorer-edit", ix))
                .h(px(ROW_HEIGHT))
                .w_full()
                .items_center()
                .gap_1()
                .pl(indent)
                .pr_2()
                .on_key_down({
                    let entity = entity.clone();
                    move |event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            cx.stop_propagation();
                            entity.update(cx, |this, cx| this.explorer_cancel_edit(cx));
                        }
                    }
                })
                .child(
                    Icon::new(if is_dir_edit {
                        IconName::FolderClosed
                    } else {
                        IconName::File
                    })
                    .size_3()
                    .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(name_input).xsmall().w_full()),
                )
                .into_any_element()
        }
        RowKind::Entry { is_dir, expanded } => {
            let path = row.path.clone();
            let is_dir = *is_dir;
            let is_selected = selection.contains(&row.path);
            let multi = is_selected && selection.len() > 1;
            // Picking up a selected row drags the whole selection.
            let drag_paths: Vec<PathBuf> = if multi {
                selection.as_ref().clone()
            } else {
                vec![path.clone()]
            };
            // A drop on a folder goes into it; on a file, next to it.
            let drop_dir: PathBuf = if is_dir {
                path.clone()
            } else {
                path.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.clone())
            };
            let name = row.name.clone();
            let chevron = if !is_dir {
                None
            } else if *expanded {
                Some(IconName::ChevronDown)
            } else {
                Some(IconName::ChevronRight)
            };
            let icon = if !is_dir {
                IconName::File
            } else if *expanded {
                IconName::FolderOpen
            } else {
                IconName::FolderClosed
            };
            h_flex()
                .id(SharedString::from(format!(
                    "explorer-row-{}",
                    path.display()
                )))
                .h(px(ROW_HEIGHT))
                .w_full()
                .items_center()
                .gap_1()
                .pl(indent)
                .pr_2()
                .text_xs()
                .cursor_pointer()
                .when(is_selected, |this| {
                    this.bg(cx.theme().list_active)
                        .text_color(cx.theme().foreground)
                })
                .when(!is_selected, |this| {
                    this.hover(|s| s.bg(cx.theme().list_hover))
                })
                .on_click({
                    let entity = entity.clone();
                    let path = path.clone();
                    move |event: &ClickEvent, _, cx| {
                        let path = path.clone();
                        let gesture = gesture_for(&event.modifiers());
                        entity.update(cx, |this, cx| {
                            this.explorer_click(path, is_dir, gesture, cx)
                        });
                    }
                })
                .on_drag(
                    DraggedEntries { paths: drag_paths },
                    |dragged: &DraggedEntries, _, _, cx| {
                        let preview = drag_preview(&dragged.paths);
                        cx.new(|_| preview)
                    },
                )
                .drag_over::<DraggedEntries>({
                    let drop_dir = drop_dir.clone();
                    move |style, dragged, _, cx| {
                        // No highlight where the drop would do nothing.
                        let pointless = dragged.paths.iter().all(|p| {
                            p.parent() == Some(drop_dir.as_path()) || drop_dir.starts_with(p)
                        });
                        if pointless {
                            style
                        } else {
                            style.bg(cx.theme().drop_target)
                        }
                    }
                })
                .on_drop::<DraggedEntries>({
                    let entity = entity.clone();
                    let drop_dir = drop_dir.clone();
                    move |dragged, _, cx| {
                        let paths = dragged.paths.clone();
                        let dest = drop_dir.clone();
                        entity.update(cx, |this, cx| this.explorer_drop(paths, dest, cx));
                    }
                })
                .child(
                    div()
                        .w(px(12.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when_some(chevron, |this, chevron| {
                            this.child(
                                Icon::new(chevron)
                                    .size_3()
                                    .text_color(cx.theme().muted_foreground),
                            )
                        }),
                )
                .child(Icon::new(icon).size_3().flex_none().text_color(if is_dir {
                    cx.theme().accent_foreground
                } else {
                    cx.theme().muted_foreground
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_ellipsis()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(name),
                )
                .context_menu({
                    let entity = entity.clone();
                    let selection = selection.clone();
                    move |menu, _, _| {
                        let mut menu = menu;
                        // Delete and Copy path act on the whole selection
                        // when this row is part of one; Rename never does.
                        let targets: Vec<PathBuf> = if multi {
                            selection.as_ref().clone()
                        } else {
                            vec![path.clone()]
                        };
                        let delete_label = if multi {
                            format!("Delete {} items…", targets.len())
                        } else {
                            "Delete…".to_string()
                        };
                        if is_dir {
                            menu = menu
                                .item(PopupMenuItem::new("New file…").on_click({
                                    let entity = entity.clone();
                                    let dir = path.clone();
                                    move |_, window, cx| {
                                        let dir = dir.clone();
                                        entity.update(cx, |this, cx| {
                                            this.explorer_begin_edit(
                                                PendingEdit::NewFile { dir },
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }))
                                .item(PopupMenuItem::new("New folder…").on_click({
                                    let entity = entity.clone();
                                    let dir = path.clone();
                                    move |_, window, cx| {
                                        let dir = dir.clone();
                                        entity.update(cx, |this, cx| {
                                            this.explorer_begin_edit(
                                                PendingEdit::NewFolder { dir },
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }))
                                .separator();
                        } else {
                            menu = menu.item(PopupMenuItem::new("Open").on_click({
                                let entity = entity.clone();
                                let path = path.clone();
                                move |_, _, cx| {
                                    let path = path.clone();
                                    entity.update(cx, |this, cx| {
                                        this.explorer_activate(path, false, cx)
                                    });
                                }
                            }));
                        }
                        if !multi {
                            menu = menu.item(PopupMenuItem::new("Rename…").on_click({
                                let entity = entity.clone();
                                let path = path.clone();
                                move |_, window, cx| {
                                    let path = path.clone();
                                    entity.update(cx, |this, cx| {
                                        this.explorer_begin_edit(
                                            PendingEdit::Rename { path },
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            }));
                        }
                        menu.item(PopupMenuItem::new(delete_label).on_click({
                            let entity = entity.clone();
                            let targets = targets.clone();
                            move |_, window, cx| {
                                let targets = targets.clone();
                                entity.update(cx, |this, cx| {
                                    this.explorer_confirm_delete(targets, window, cx)
                                });
                            }
                        }))
                        .separator()
                        .item(PopupMenuItem::new("Reveal in file manager").on_click({
                            let path = path.clone();
                            move |_, _, cx| {
                                super::artifact_card::reveal_path_in_os(&path, cx);
                            }
                        }))
                        .item(
                            PopupMenuItem::new(if multi { "Copy paths" } else { "Copy path" })
                                .on_click({
                                    let targets = targets.clone();
                                    move |_, _, cx| {
                                        let text = targets
                                            .iter()
                                            .map(|p| p.display().to_string())
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    }
                                }),
                        )
                    }
                })
                .into_any_element()
        }
    }
}
