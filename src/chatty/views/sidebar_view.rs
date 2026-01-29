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

/// Sidebar view showing conversations
pub struct SidebarView {
    conversations: Vec<(String, String, Option<f64>)>, // (id, title, cost)
    active_conversation_id: Option<String>,
    on_new_chat: Option<NewChatCallback>,
    on_settings: Option<SettingsCallback>,
    on_select_conversation: Option<SelectConversationCallback>,
    on_delete_conversation: Option<DeleteConversationCallback>,
    is_collapsed: bool,
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
            is_collapsed: false,
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

        let width = if self.is_collapsed { px(48.) } else { px(255.) };

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
            .border_r_1()
            .when(self.is_collapsed, |this| this.gap_2())
            .child(
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
            .child(
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
                                                ConversationItem::new(id.clone(), title.clone())
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
                                                ix == self.conversations.len().saturating_sub(1),
                                                |this| this.mb_3(),
                                            )
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                    ),
            )
            .child(
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
    }
}
