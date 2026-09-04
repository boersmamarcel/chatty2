use crate::chatty::controllers::ChattyApp;
use crate::chatty::views::AppTitleBar;
use crate::chatty::views::footer::StatusFooterView;
use crate::chatty::views::transcript::ArtifactMode;
use crate::settings::models::general_model::GeneralSettingsModel;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::menu::PopupMenuItem;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Root, Sizable,
    button::{Button, ButtonVariants, DropdownButton},
};

impl Render for ChattyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::boot_timing::checkpoint("open_to_first_frame");

        let dialog_layer = Root::render_dialog_layer(window, cx);
        let sidebar = self.sidebar_view.clone();
        let chat_view = self.chat_view.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_size(px(cx.global::<GeneralSettingsModel>().font_size))
            .relative() // Enable absolute positioning for floating button
            .child(
                // Custom titlebar with toggle button
                AppTitleBar::new(self.sidebar_view.clone(), self.chat_view.clone()),
            )
            .child(
                // Content area - existing panels
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(
                        // Sidebar - left panel
                        self.sidebar_view.clone(),
                    )
                    .child(
                        // Chat view - right panel
                        self.chat_view.clone(),
                    ),
            )
            .child(
                // Footer bar
                StatusFooterView::new(),
            )
            // Floating toggle button for macOS (rendered last = on top)
            .when(cfg!(target_os = "macos"), |this| {
                let is_collapsed = sidebar.read(cx).is_collapsed();
                this.child(
                    div().absolute().top(px(8.)).left(px(80.)).child(
                        Button::new("toggle-sidebar-floating")
                            .icon(Icon::new(if is_collapsed {
                                IconName::PanelLeftOpen
                            } else {
                                IconName::PanelLeftClose
                            }))
                            .label("")
                            .small()
                            .on_click({
                                let sidebar = sidebar.clone();
                                move |_event, _window, cx| {
                                    sidebar.update(cx, |sidebar, cx| {
                                        sidebar.toggle_collapsed(cx);
                                    });
                                }
                            }),
                    ),
                )
                .child(
                    div().absolute().top(px(8.)).left(px(128.)).child(
                        Button::new("search-conversations-floating")
                            .icon(Icon::new(IconName::Search))
                            .label("")
                            .small()
                            .tooltip("Search conversations")
                            .on_click(|_event, window, cx| {
                                crate::chatty::views::SearchConversationsDialog::open(window, cx);
                            }),
                    ),
                )
                .when(
                    chat_view.read(cx).artifact_view().read(cx).mode == ArtifactMode::Closed,
                    |this| {
                        // Unfold the artifact panel, mirroring the sidebar's own
                        // floating toggle. Only while the panel is closed: once
                        // open, its header occupies this same corner (maximize,
                        // close) and the button would sit on top of those. The
                        // caret opens a small picker for manually starting an
                        // artifact — "Browser" for now — so the panel is reachable
                        // even when the agent never opened one.
                        this.child(
                            div().absolute().top(px(8.)).right(px(8.)).child(
                                DropdownButton::new("toggle-artifact-floating")
                                    .small()
                                    .button(
                                        Button::new("toggle-artifact-floating-main")
                                            .ghost()
                                            .icon(Icon::new(IconName::PanelRightOpen))
                                            .label("")
                                            .small()
                                            .tooltip("Open artifact panel")
                                            .on_click({
                                                let chat_view = chat_view.clone();
                                                move |_event, _window, cx| {
                                                    chat_view.update(cx, |view, cx| {
                                                        view.toggle_artifact_panel(cx);
                                                    });
                                                }
                                            }),
                                    )
                                    .dropdown_menu({
                                        let chat_view = chat_view.clone();
                                        move |menu, _window, _cx| {
                                            menu.item(PopupMenuItem::new("Browser").on_click({
                                                let chat_view = chat_view.clone();
                                                move |_event, _window, cx| {
                                                    chat_view.update(cx, |view, cx| {
                                                        view.open_manual_browser(cx);
                                                    });
                                                }
                                            }))
                                        }
                                    }),
                            ),
                        )
                    },
                )
            })
            .children(dialog_layer)
    }
}
