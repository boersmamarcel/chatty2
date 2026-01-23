use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use futures::stream::BoxStream;
use rig::OneOrMany;
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::completion::Message;
use rig::completion::Prompt;
use rig::completion::message::{AssistantContent, Text};
use rig::message::UserContent;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::chatty::repositories::ConversationData;
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

/// Enum-based agent wrapper for multi-provider support
#[derive(Clone)]
pub enum AgentClient {
    Anthropic(Agent<rig::providers::anthropic::completion::CompletionModel>),
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel>),
    Gemini(Agent<rig::providers::gemini::completion::CompletionModel>),
    Cohere(Agent<rig::providers::cohere::completion::CompletionModel>),
    Ollama(Agent<rig::providers::ollama::CompletionModel>),
}

impl AgentClient {
    /// Create AgentClient from ModelConfig and ProviderConfig
    pub async fn from_model_config(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        let api_key = provider_config.api_key.clone();
        let base_url = provider_config.base_url.clone();

        match &provider_config.provider_type {
            ProviderType::Anthropic => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Anthropic provider"))?;

                let client = rig::providers::anthropic::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                Ok(AgentClient::Anthropic(builder.build()))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::OpenAI(builder.build()))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::Gemini(builder.build()))
            }
            ProviderType::Cohere => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Cohere provider"))?;

                let client = rig::providers::cohere::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                Ok(AgentClient::Cohere(builder.build()))
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(&url)
                    .build()?;

                let builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                Ok(AgentClient::Ollama(builder.build()))
            }
            _ => Err(anyhow!(
                "Unsupported provider type: {:?}",
                provider_config.provider_type
            )),
        }
    }
}

/// Stream chunks emitted during responses
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Text(String),
    ToolCallStarted { id: String, name: String },
    ToolCallInput { id: String, arguments: String },
    ToolCallResult { id: String, result: String },
    ToolCallError { id: String, error: String },
    Done,
    Error(String),
}

/// Type alias for response streams
pub type ResponseStream = BoxStream<'static, Result<StreamChunk>>;

/// Helper macro to process agent streams
macro_rules! process_agent_stream {
    ($stream:expr) => {
        Box::pin(async_stream::stream! {
            while let Some(item) = $stream.next().await {
                match item {
                    Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(content)) => {
                        match content {
                            rig::streaming::StreamedAssistantContent::Text(text) => {
                                yield Ok(StreamChunk::Text(text.text));
                            }
                            rig::streaming::StreamedAssistantContent::ToolCall(tool_call) => {
                                let tool_id = tool_call.call_id.clone()
                                    .unwrap_or_else(|| tool_call.id.clone());
                                yield Ok(StreamChunk::ToolCallStarted {
                                    id: tool_id.clone(),
                                    name: tool_call.function.name.clone(),
                                });
                                yield Ok(StreamChunk::ToolCallInput {
                                    id: tool_id,
                                    arguments: serde_json::to_string(&tool_call.function.arguments)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                });
                            }
                            _ => {}
                        }
                    }
                    Ok(rig::agent::MultiTurnStreamItem::StreamUserItem(user_content)) => {
                        use rig::streaming::StreamedUserContent;
                        use rig::completion::message::ToolResultContent;

                        if let StreamedUserContent::ToolResult(tool_result) = user_content {
                            let content_text = tool_result.content.iter()
                                .filter_map(|c| match c {
                                    ToolResultContent::Text(text) => Some(text.text.clone()),
                                    ToolResultContent::Image(_) => Some("[Image result]".to_string()),
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            let call_id = tool_result.call_id.clone()
                                .unwrap_or_else(|| tool_result.id.clone());

                            let is_error = content_text.trim_start().starts_with("Error:")
                                || content_text.trim_start().starts_with("ERROR:")
                                || content_text.trim_start().starts_with("error:");

                            if is_error {
                                yield Ok(StreamChunk::ToolCallError {
                                    id: call_id,
                                    error: content_text,
                                });
                            } else {
                                yield Ok(StreamChunk::ToolCallResult {
                                    id: call_id,
                                    result: content_text,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        yield Ok(StreamChunk::Error(e.to_string()));
                        return;
                    }
                    _ => {}
                }
            }
            yield Ok(StreamChunk::Done);
        })
    };
}

/// Stream a prompt with an agent
pub async fn stream_prompt(
    agent: &AgentClient,
    history: &[Message],
    contents: Vec<UserContent>,
) -> Result<(ResponseStream, Message)> {
    use rig::streaming::StreamingPrompt;

    let user_message = Message::User {
        content: OneOrMany::many(contents).context("Failed to create message from contents")?,
    };

    let history_snapshot = history.to_vec();

    let stream: ResponseStream = match agent {
        AgentClient::Anthropic(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::OpenAI(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::Gemini(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::Cohere(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::Ollama(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
    };

    Ok((stream, user_message))
}

/// Extract text from UserContent
fn extract_text_from_user_content(content: &UserContent) -> Option<String> {
    match content {
        UserContent::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

/// Extract text from AssistantContent
fn extract_text_from_assistant_content(content: &AssistantContent) -> Option<String> {
    match content {
        AssistantContent::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

/// Truncate text to max length
fn truncate_text(text: &str, max_len: usize) -> String {
    text.chars().take(max_len).collect()
}

/// Clean and validate generated title
fn clean_title(raw_title: &str) -> String {
    let cleaned = raw_title
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .lines()
        .next()
        .unwrap_or("New Chat")
        .to_string();

    if cleaned.len() > 100 {
        format!("{}...", &cleaned[..97])
    } else if cleaned.is_empty() {
        "New Chat".to_string()
    } else {
        cleaned
    }
}

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
    ///
    /// # Arguments
    /// * `data` - The persisted conversation data
    /// * `model_config` - The model configuration to use
    /// * `provider_config` - The provider configuration to use
    ///
    /// # Errors
    /// Returns an error if:
    /// - Agent creation fails
    /// - Message history deserialization fails
    /// - System traces deserialization fails
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

    /// Extract text from first user message
    fn extract_first_user_text(&self) -> String {
        match self.history.first() {
            Some(Message::User { content, .. }) => content
                .iter()
                .find_map(|c| extract_text_from_user_content(c))
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Extract text from first assistant message
    fn extract_first_assistant_text(&self) -> String {
        match self.history.get(1) {
            Some(Message::Assistant { content, .. }) => content
                .iter()
                .find_map(|c| extract_text_from_assistant_content(c))
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Generate a concise title for the conversation based on the first exchange
    pub async fn generate_and_set_title(&mut self) -> Result<String> {
        eprintln!("🎯 [Conversation] generate_and_set_title called");
        // Guard: Only generate title if we have exactly 2 messages
        if self.message_count() != 2 {
            let err_msg = format!(
                "Title generation requires exactly 2 messages, found {}",
                self.message_count()
            );
            eprintln!("❌ [Conversation] {}", err_msg);
            return Err(anyhow!(err_msg));
        }

        eprintln!("✓ [Conversation] Message count is 2, proceeding");

        // Extract first exchange
        let user_text = self.extract_first_user_text();
        let assistant_text = self.extract_first_assistant_text();

        eprintln!(
            "📝 [Conversation] User: {} chars, Assistant: {} chars",
            user_text.len(),
            assistant_text.len()
        );

        // Build title generation prompt
        let title_prompt = format!(
            "Generate a concise, descriptive title (3-7 words) for this conversation. \
            Output ONLY the title, no quotes, no explanation.\n\n\
            User: {}\n\nAssistant: {}",
            truncate_text(&user_text, 500),
            truncate_text(&assistant_text, 500)
        );

        // Use agent.prompt() for non-streaming completion
        // The Prompt trait returns a String directly
        eprintln!("🤖 [Conversation] Calling LLM for title generation...");
        let response_text = match &self.agent {
            AgentClient::Anthropic(agent) => agent.prompt(&title_prompt).await?,
            AgentClient::OpenAI(agent) => agent.prompt(&title_prompt).await?,
            AgentClient::Gemini(agent) => agent.prompt(&title_prompt).await?,
            AgentClient::Cohere(agent) => agent.prompt(&title_prompt).await?,
            AgentClient::Ollama(agent) => agent.prompt(&title_prompt).await?,
        };

        eprintln!("📨 [Conversation] LLM response: '{}'", response_text);

        // Clean and validate the title
        let title = clean_title(&response_text);

        eprintln!("🧹 [Conversation] Cleaned title: '{}'", title);

        // Set the title
        self.set_title(title.clone());

        eprintln!("✅ [Conversation] Title set successfully");

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
    ///
    /// This allows switching models mid-conversation.
    /// The history is preserved but future messages will use the new model.
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
