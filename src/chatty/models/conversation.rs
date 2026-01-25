use anyhow::{Context, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rig::OneOrMany;
use rig::completion::Message;
use rig::completion::message::{AssistantContent, Text};
use rig::message::UserContent;

use crate::chatty::factories::AgentClient;
use crate::chatty::repositories::ConversationData;
use crate::chatty::services::{ResponseStream, generate_title, stream_prompt};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderConfig;

/// A single conversation with an AI agent
pub struct Conversation {
    id: String,
    title: String,
    model_id: String,
    agent: AgentClient,
    history: Vec<Message>,
    system_traces: Vec<Option<serde_json::Value>>,
    created_at: SystemTime,
    updated_at: SystemTime,
}

impl Conversation {
    /// Create a new conversation from model and provider config
    pub async fn new(
        id: String,
        title: String,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        let agent = AgentClient::from_model_config(model_config, provider_config)
            .await
            .context("Failed to create agent from config")?;

        let now = SystemTime::now();

        Ok(Self {
            id,
            title,
            model_id: model_config.id.clone(),
            agent,
            history: Vec::new(),
            system_traces: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Restore a conversation from persisted data
    pub async fn from_data(
        data: ConversationData,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        // Reconstruct agent
        let agent = AgentClient::from_model_config(model_config, provider_config)
            .await
            .context("Failed to create agent from config")?;

        // Deserialize message history
        let history = Self::deserialize_history(&data.message_history)
            .context("Failed to deserialize message history")?;

        // Deserialize system traces
        let system_traces = Self::deserialize_traces(&data.system_traces)
            .context("Failed to deserialize system traces")?;

        // Convert Unix timestamps to SystemTime
        let created_at = UNIX_EPOCH + Duration::from_secs(data.created_at as u64);
        let updated_at = UNIX_EPOCH + Duration::from_secs(data.updated_at as u64);

        Ok(Self {
            id: data.id,
            title: data.title,
            model_id: data.model_id,
            agent,
            history,
            system_traces,
            created_at,
            updated_at,
        })
    }

    /// Send a text message and get a streaming response
    pub async fn send_text(&mut self, text: String) -> Result<ResponseStream> {
        let content = UserContent::Text(Text { text });
        self.send_multimodal(vec![content]).await
    }

    /// Add user message to history
    pub fn add_user_message_to_history(&mut self, message: Message) {
        self.history.push(message);
        self.system_traces.push(None);
        self.updated_at = SystemTime::now();
    }

    /// Send a multimodal message (text + images/PDFs)
    pub async fn send_multimodal(&mut self, contents: Vec<UserContent>) -> Result<ResponseStream> {
        let (stream, user_message) = stream_prompt(&self.agent, &self.history, contents).await?;
        self.add_user_message_to_history(user_message);
        Ok(stream)
    }

    /// Finalize response after stream is consumed
    pub fn finalize_response(&mut self, response_text: String) {
        let assistant_message = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: response_text,
            })),
        };

        self.history.push(assistant_message);
        self.updated_at = SystemTime::now();
    }

    /// Add a trace for the most recent message
    pub fn add_trace(&mut self, trace: Option<serde_json::Value>) {
        self.system_traces.push(trace);
        self.updated_at = SystemTime::now();
    }

    /// Get conversation ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get conversation title
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Set conversation title
    pub fn set_title(&mut self, title: String) {
        self.title = title;
        self.updated_at = SystemTime::now();
    }

    /// Get model ID
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Get the complete conversation history
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Get system traces
    pub fn system_traces(&self) -> &[Option<serde_json::Value>] {
        &self.system_traces
    }

    /// Get creation timestamp
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// Get last update timestamp
    pub fn updated_at(&self) -> SystemTime {
        self.updated_at
    }

    /// Clear conversation history
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.system_traces.clear();
        self.updated_at = SystemTime::now();
    }

    /// Get the count of messages in history
    pub fn message_count(&self) -> usize {
        self.history.len()
    }

    /// Generate and set a concise title for the conversation based on the first exchange
    pub async fn generate_and_set_title(&mut self) -> Result<String> {
        let title = generate_title(&self.agent, &self.history).await?;
        self.set_title(title.clone());
        Ok(title)
    }

    /// Serialize message history to JSON string
    pub fn serialize_history(&self) -> Result<String> {
        serde_json::to_string(&self.history).context("Failed to serialize message history")
    }

    /// Deserialize message history from JSON string
    pub fn deserialize_history(json: &str) -> Result<Vec<Message>> {
        serde_json::from_str(json).context("Failed to deserialize message history")
    }

    /// Serialize system traces to JSON string
    pub fn serialize_traces(&self) -> Result<String> {
        serde_json::to_string(&self.system_traces).context("Failed to serialize system traces")
    }

    /// Deserialize system traces from JSON string
    pub fn deserialize_traces(json: &str) -> Result<Vec<Option<serde_json::Value>>> {
        serde_json::from_str(json).context("Failed to deserialize system traces")
    }

    /// Get the agent
    pub fn agent(&self) -> &AgentClient {
        &self.agent
    }

    /// Update the model and agent for this conversation
    pub async fn update_model(
        &mut self,
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<()> {
        let agent = AgentClient::from_model_config(model_config, provider_config)
            .await
            .context("Failed to create agent from config")?;

        self.agent = agent;
        self.model_id = model_config.id.clone();
        self.updated_at = SystemTime::now();

        Ok(())
    }
}
