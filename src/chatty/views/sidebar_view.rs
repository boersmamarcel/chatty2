use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    ActiveTheme, Collapsible, Icon, IconName, Sizable, button::Button, h_flex, v_flex,
};
use std::sync::Arc;

use super::conversation_item::ConversationItem;

/// Callback types for sidebar actions
pub type NewChatCallback = Arc<dyn Fn(&mut App) + Send + Sync>;
pub type SettingsCallback = Arc<dyn Fn(&mut App) + Send + Sync>;
pub type SelectConversationCallback = Arc<dyn Fn(&str, &mut App) + Send + Sync>;
pub type DeleteConversationCallback = Arc<dyn Fn(&str, &mut App) + Send + Sync>;
pub type ToggleCallback = Arc<dyn Fn(bool, &mut App) + Send + Sync>;
pub type LoadMoreCallback = Arc<dyn Fn(&mut App) + Send + Sync>;

/// Sidebar view showing conversations
pub struct SidebarView {
    conversations: Vec<(String, String, Option<f64>)>, // (id, title, cost)
    active_conversation_id: Option<String>,
    on_new_chat: Option<NewChatCallback>,
    on_settings: Option<SettingsCallback>,
    on_select_conversation: Option<SelectConversationCallback>,
    on_delete_conversation: Option<DeleteConversationCallback>,
    on_toggle: Option<ToggleCallback>,
    on_load_more: Option<LoadMoreCallback>,
    is_collapsed: bool,
    // OPTIMIZATION: Pagination for sidebar
    visible_limit: usize, // How many conversations to show (starts at 20)
    total_count: usize,   // Total available conversations
}

impl SidebarView {
    pub fn new() -> Self {
        Self {
            conversations: Vec::new(),
            active_conversation_id: None,
            on_new_chat: None,
            on_settings: None,
            on_select_conversation: None,
            on_delete_conversation: None,
            on_toggle: None,
            on_load_more: None,
            is_collapsed: false,
            visible_limit: 20, // Start with 20 conversations
            total_count: 0,
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

    /// Set callback for new chat button
    pub fn set_on_new_chat<F>(&mut self, callback: F)
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_new_chat = Some(Arc::new(callback));
    }

    /// Set callback for settings button
    pub fn set_on_settings<F>(&mut self, callback: F)
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_settings = Some(Arc::new(callback));
    }

    /// Set callback for selecting a conversation
    pub fn set_on_select_conversation<F>(&mut self, callback: F)
    where
        F: Fn(&str, &mut App) + Send + Sync + 'static,
    {
        self.on_select_conversation = Some(Arc::new(callback));
    }

    /// Set callback for deleting a conversation
    pub fn set_on_delete_conversation<F>(&mut self, callback: F)
    where
        F: Fn(&str, &mut App) + Send + Sync + 'static,
    {
        self.on_delete_conversation = Some(Arc::new(callback));
    }

    /// Set callback for toggling sidebar collapse state
    pub fn set_on_toggle<F>(&mut self, callback: F)
    where
        F: Fn(bool, &mut App) + Send + Sync + 'static,
    {
        self.on_toggle = Some(Arc::new(callback));
    }

    /// Set callback for loading more conversations
    pub fn set_on_load_more<F>(&mut self, callback: F)
    where
        F: Fn(&mut App) + Send + Sync + 'static,
    {
        self.on_load_more = Some(Arc::new(callback));
    }

    /// Toggle the collapsed state of the sidebar
    pub fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.is_collapsed = !self.is_collapsed;
        if let Some(callback) = &self.on_toggle {
            callback(self.is_collapsed, cx);
        }
        cx.notify();
    }

    /// Set the collapsed state of the sidebar
    #[allow(dead_code)]
    pub fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.is_collapsed = collapsed;
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
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tracing::debug!(
            count = self.conversations.len(),
            "SidebarView: render called with {} conversations",
            self.conversations.len()
        );

        let on_new_chat = self.on_new_chat.clone();
        let on_settings = self.on_settings.clone();
        let on_select = self.on_select_conversation.clone();
        let on_delete = self.on_delete_conversation.clone();
        let active_id = self.active_conversation_id.clone();

        let width = if self.is_collapsed { px(0.) } else { px(255.) };

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
            .when(!self.is_collapsed, |this| {
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
                                .label(if self.is_collapsed { "+" } else { "New Chat" })
                                .small()
                                .w_full()
                                .on_click(move |_event, _window, cx| {
                                    if let Some(callback) = &on_new_chat {
                                        callback(cx);
                                    }
                                }),
                        ),
                )
            })
            .when(!self.is_collapsed, |this| {
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
                                            let on_select_clone = on_select.clone();
                                            let on_delete_clone = on_delete.clone();

                                            div()
                                                .id(ix)
                                                .child(
                                                    ConversationItem::new(
                                                        id.clone(),
                                                        title.clone(),
                                                    )
                                                    .active(is_active)
                                                    .collapsed(self.is_collapsed)
                                                    .cost(*cost)
                                                    .on_click(move |conv_id, cx| {
                                                        if let Some(callback) = &on_select_clone {
                                                            callback(conv_id, cx);
                                                        }
                                                    })
                                                    .on_delete(move |conv_id, cx| {
                                                        if let Some(callback) = &on_delete_clone {
                                                            callback(conv_id, cx);
                                                        }
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
                                                    let on_load_more = self.on_load_more.clone();
                                                    let sidebar_entity = cx.entity();
                                                    move |_event, _window, cx| {
                                                        // Increase visible limit
                                                        sidebar_entity.update(cx, |sidebar, cx| {
                                                            sidebar.load_more(cx);
                                                        });
                                                        // Trigger callback to reload conversations with new limit
                                                        if let Some(callback) = &on_load_more {
                                                            callback(cx);
                                                        }
                                                    }
                                                }),
                                        ),
                                    )
                                }),
                        ),
                )
            })
            .when(!self.is_collapsed, |this| {
                this.child(
                    // Footer: Settings button
                    h_flex()
                        .id("footer")
                        .pb_3()
                        .px_3()
                        .gap_2()
                        .when(self.is_collapsed, |this| this.pt_2().px_2())
                        .child(
                            Button::new("settings")
                                .icon(Icon::new(IconName::Settings))
                                .label(if self.is_collapsed { "" } else { "Settings" })
                                .small()
                                .w_full()
                                .on_click(move |_event, _window, cx| {
                                    if let Some(callback) = &on_settings {
                                        callback(cx);
                                    }
                                }),
                        ),
                )
            })
    }
}
