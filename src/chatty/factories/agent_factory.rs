use anyhow::{Result, anyhow};
use rig::agent::Agent;
use rig::client::CompletionClient;

use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

/// Enum-based agent wrapper for multi-provider support
#[derive(Clone)]
pub enum AgentClient {
    Anthropic(Agent<rig::providers::anthropic::completion::CompletionModel>),
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel>),
    Gemini(Agent<rig::providers::gemini::completion::CompletionModel>),
    Mistral(Agent<rig::providers::mistral::completion::CompletionModel>),
    Ollama(Agent<rig::providers::ollama::CompletionModel>),
}

impl AgentClient {
    /// Create AgentClient from ModelConfig and ProviderConfig
    pub async fn from_model_config(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        Self::from_model_config_with_tools(model_config, provider_config, None).await
    }

    /// Create AgentClient from ModelConfig and ProviderConfig with optional MCP tools
    pub async fn from_model_config_with_tools(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
        mcp_tools: Option<Vec<(Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
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

                // Add MCP tools if available and build
                let agent = if let Some(tools_list) = mcp_tools {
                    // Combine all tools from all servers
                    let mut all_tools = Vec::new();
                    let mut server_sink = None;
                    
                    for (tools, sink) in tools_list {
                        all_tools.extend(tools);
                        if server_sink.is_none() {
                            server_sink = Some(sink);
                        }
                    }
                    
                    if !all_tools.is_empty() {
                        if let Some(sink) = server_sink {
                            builder.rmcp_tools(all_tools.clone(), sink.clone()).build()
                        } else {
                            builder.build()
                        }
                    } else {
                        builder.build()
                    }
                } else {
                    builder.build()
                };

                Ok(AgentClient::Anthropic(agent))
            }
            ProviderType::OpenAI => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for OpenAI provider"))?;

                let client = rig::providers::openai::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble);

                // Only set temperature if the model supports it
                if model_config.supports_temperature {
                    builder = builder.temperature(model_config.temperature as f64);
                }

                // Add MCP tools if available and build
                let agent = if let Some(tools_list) = mcp_tools {
                    let mut all_tools = Vec::new();
                    let mut server_sink = None;
                    
                    for (tools, sink) in tools_list {
                        all_tools.extend(tools);
                        if server_sink.is_none() {
                            server_sink = Some(sink);
                        }
                    }
                    
                    if !all_tools.is_empty() {
                        if let Some(sink) = server_sink {
                            builder.rmcp_tools(all_tools.clone(), sink.clone()).build()
                        } else {
                            builder.build()
                        }
                    } else {
                        builder.build()
                    }
                } else {
                    builder.build()
                };

                Ok(AgentClient::OpenAI(agent))
            }
            ProviderType::Gemini => {
                let key =
                    api_key.ok_or_else(|| anyhow!("API key not configured for Gemini provider"))?;

                let client = rig::providers::gemini::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                // Add MCP tools if available and build
                let agent = if let Some(tools_list) = mcp_tools {
                    let mut all_tools = Vec::new();
                    let mut server_sink = None;
                    
                    for (tools, sink) in tools_list {
                        all_tools.extend(tools);
                        if server_sink.is_none() {
                            server_sink = Some(sink);
                        }
                    }
                    
                    if !all_tools.is_empty() {
                        if let Some(sink) = server_sink {
                            builder.rmcp_tools(all_tools.clone(), sink.clone()).build()
                        } else {
                            builder.build()
                        }
                    } else {
                        builder.build()
                    }
                } else {
                    builder.build()
                };

                Ok(AgentClient::Gemini(agent))
            }
            ProviderType::Mistral => {
                let key = api_key
                    .ok_or_else(|| anyhow!("API key not configured for Mistral provider"))?;

                let client = rig::providers::mistral::Client::new(&key)?;
                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                if let Some(max_tokens) = model_config.max_tokens {
                    builder = builder.max_tokens(max_tokens as u64);
                }

                // Add MCP tools if available and build
                let agent = if let Some(tools_list) = mcp_tools {
                    let mut all_tools = Vec::new();
                    let mut server_sink = None;
                    
                    for (tools, sink) in tools_list {
                        all_tools.extend(tools);
                        if server_sink.is_none() {
                            server_sink = Some(sink);
                        }
                    }
                    
                    if !all_tools.is_empty() {
                        if let Some(sink) = server_sink {
                            builder.rmcp_tools(all_tools.clone(), sink.clone()).build()
                        } else {
                            builder.build()
                        }
                    } else {
                        builder.build()
                    }
                } else {
                    builder.build()
                };

                Ok(AgentClient::Mistral(agent))
            }
            ProviderType::Ollama => {
                let url = base_url.unwrap_or_else(|| "http://localhost:11434".to_string());

                let client = rig::providers::ollama::Client::builder()
                    .api_key(rig::client::Nothing)
                    .base_url(&url)
                    .build()?;

                let mut builder = client
                    .agent(&model_config.model_identifier)
                    .preamble(&model_config.preamble)
                    .temperature(model_config.temperature as f64);

                // Add MCP tools if available and build
                let agent = if let Some(tools_list) = mcp_tools {
                    let mut all_tools = Vec::new();
                    let mut server_sink = None;
                    
                    for (tools, sink) in tools_list {
                        all_tools.extend(tools);
                        if server_sink.is_none() {
                            server_sink = Some(sink);
                        }
                    }
                    
                    if !all_tools.is_empty() {
                        if let Some(sink) = server_sink {
                            builder.rmcp_tools(all_tools.clone(), sink.clone()).build()
                        } else {
                            builder.build()
                        }
                    } else {
                        builder.build()
                    }
                } else {
                    builder.build()
                };

                Ok(AgentClient::Ollama(agent))
            }
        }
    }

    /// Returns the provider name for logging/debugging.
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &'static str {
        match self {
            AgentClient::Anthropic(_) => "Anthropic",
            AgentClient::OpenAI(_) => "OpenAI",
            AgentClient::Gemini(_) => "Gemini",
            AgentClient::Ollama(_) => "Ollama",
            AgentClient::Mistral(_) => "Mistral",
        }
    }
}
