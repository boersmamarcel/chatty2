use crate::chatty::models::conversation::{Conversation, MessageRole};
use gpui::*;

pub struct ChattyApp {
    pub conversations: Vec<Conversation>,
    pub selected_conversation_id: Option<String>,
}

impl ChattyApp {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        // Create some sample conversations for demonstration
        let mut sample_conversations = Vec::new();

        let mut conv1 = Conversation::new("Getting Started with Rust".to_string());
        conv1.add_message(MessageRole::User, "What is Rust?".to_string());
        conv1.add_message(
            MessageRole::Assistant,
            "Rust is a systems programming language focused on safety, speed, and concurrency.".to_string(),
        );
        sample_conversations.push(conv1);

        let mut conv2 = Conversation::new("GPUI Components Tutorial".to_string());
        conv2.add_message(MessageRole::User, "How do I create a custom component in GPUI?".to_string());
        sample_conversations.push(conv2);

        let mut conv3 = Conversation::new("Async Programming Patterns".to_string());
        conv3.add_message(MessageRole::User, "Explain async/await in Rust".to_string());
        sample_conversations.push(conv3);

        Self {
            conversations: sample_conversations,
            selected_conversation_id: None,
        }
    }
}
