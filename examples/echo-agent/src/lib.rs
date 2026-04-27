//! Echo Agent — example chatty WASM module.
//!
//! A simple but complete module demonstrating:
//! - **Chat via host LLM** — delegates to the host LLM for intelligent responses
//! - **Tool calling** — exposes a `transform_text` tool for text operations
//! - **Agent card** — proper metadata for discovery and invocation
//!
//! ## Usage
//!
//! Via chatty chat (after installing from Hive marketplace):
//! ```text
//! Use the echo-agent to process: Hello World
//! ```
//!
//! Via the `/agent` command:
//! ```text
//! /agent echo-agent Transform "hello world" to uppercase
//! ```
//!
//! ## Tools
//!
//! | Tool | Input | Output |
//! |------|-------|--------|
//! | `transform_text` | `text`, `operation` | Transformed text |
//!
//! Supported operations: `uppercase`, `lowercase`, `reverse`, `word_count`,
//! `char_count`, `title_case`, `snake_case`.

use chatty_module_sdk::{
    export_module, AgentCard, ChatRequest, ChatResponse, Message, ModuleExports, Role, Skill,
    ToolDefinition,
};

const TOOLS_JSON: &str = r#"[
  {
    "name": "transform_text",
    "description": "Transform text using a specified operation. Supported operations: uppercase, lowercase, reverse, word_count, char_count, title_case, snake_case.",
    "parameters": {
      "type": "object",
      "properties": {
        "text": {
          "type": "string",
          "description": "The text to transform."
        },
        "operation": {
          "type": "string",
          "description": "The transformation to apply.",
          "enum": ["uppercase", "lowercase", "reverse", "word_count", "char_count", "title_case", "snake_case"]
        }
      },
      "required": ["text", "operation"]
    }
  }
]"#;

const MAX_TURNS: usize = 4;

#[derive(Default)]
pub struct EchoAgent;

impl ModuleExports for EchoAgent {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        chatty_module_sdk::log::info("echo-agent: handling chat request");

        let system_prompt = Message {
            role: Role::System,
            content: "You are Echo Agent, a helpful text processing assistant. \
                      You have a `transform_text` tool that can uppercase, lowercase, reverse, \
                      count words, count characters, title-case, or snake-case text. \
                      When asked to transform text, use the tool. Otherwise respond directly. \
                      Be concise and helpful."
                .to_string(),
        };

        let mut messages: Vec<Message> = vec![system_prompt];
        messages.extend(req.messages.iter().cloned());

        // Use configured model or empty string (host picks default)
        let model = chatty_module_sdk::config::get("model").unwrap_or_default();

        // Agentic loop — let the LLM call tools and feed results back
        for turn in 0..MAX_TURNS {
            chatty_module_sdk::log::info(&format!("echo-agent: LLM turn {turn}"));

            let resp = chatty_module_sdk::llm::complete(
                &model,
                &messages,
                Some(TOOLS_JSON),
            )?;

            if resp.tool_calls.is_empty() {
                return Ok(ChatResponse {
                    content: resp.content,
                    tool_calls: vec![],
                    usage: resp.usage,
                });
            }

            // LLM requested tool calls — execute them
            messages.push(Message {
                role: Role::Assistant,
                content: resp.content.clone(),
            });

            for tc in &resp.tool_calls {
                chatty_module_sdk::log::info(&format!(
                    "echo-agent: tool call {}({})",
                    tc.name, tc.arguments
                ));

                let result = self.invoke_tool(tc.name.clone(), tc.arguments.clone());
                let result_text = match result {
                    Ok(r) => r,
                    Err(e) => format!("{{\"error\": \"{e}\"}}"),
                };

                messages.push(Message {
                    role: Role::User,
                    content: format!(
                        "[Tool result for {}]: {}",
                        tc.name, result_text
                    ),
                });
            }
        }

        // Fallback if we hit max turns — ask LLM to summarize
        let resp = chatty_module_sdk::llm::complete(&model, &messages, None)?;
        Ok(ChatResponse {
            content: resp.content,
            tool_calls: vec![],
            usage: resp.usage,
        })
    }

    fn invoke_tool(&self, name: String, args: String) -> Result<String, String> {
        match name.as_str() {
            "transform_text" => {
                let v: serde_json::Value =
                    serde_json::from_str(&args).map_err(|e| format!("Invalid JSON: {e}"))?;
                let text = v
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing 'text' parameter")?;
                let op = v
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing 'operation' parameter")?;

                let result = match op {
                    "uppercase" => text.to_uppercase(),
                    "lowercase" => text.to_lowercase(),
                    "reverse" => text.chars().rev().collect(),
                    "word_count" => {
                        let count = text.split_whitespace().count();
                        format!("{count}")
                    }
                    "char_count" => {
                        let count = text.chars().count();
                        format!("{count}")
                    }
                    "title_case" => text
                        .split_whitespace()
                        .map(|word| {
                            let mut chars = word.chars();
                            match chars.next() {
                                None => String::new(),
                                Some(c) => {
                                    c.to_uppercase().to_string() + &chars.as_str().to_lowercase()
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    "snake_case" => text
                        .split_whitespace()
                        .map(|w| w.to_lowercase())
                        .collect::<Vec<_>>()
                        .join("_"),
                    _ => return Err(format!("Unknown operation: {op}")),
                };

                Ok(serde_json::json!({
                    "input": text,
                    "operation": op,
                    "result": result
                })
                .to_string())
            }
            _ => Err(format!("Unknown tool: {name}")),
        }
    }

    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "transform_text".to_string(),
            description: "Transform text using a specified operation (uppercase, lowercase, \
                          reverse, word_count, char_count, title_case, snake_case)."
                .to_string(),
            parameters_schema: r#"{"type":"object","properties":{"text":{"type":"string","description":"The text to transform."},"operation":{"type":"string","description":"The transformation to apply.","enum":["uppercase","lowercase","reverse","word_count","char_count","title_case","snake_case"]}},"required":["text","operation"]}"#.to_string(),
        }]
    }

    fn get_agent_card(&self) -> AgentCard {
        AgentCard {
            name: "echo-agent".to_string(),
            display_name: "Echo Agent".to_string(),
            description: "A text processing agent that can transform text (uppercase, lowercase, \
                          reverse, etc.) and chat about text-related tasks."
                .to_string(),
            version: "0.1.0".to_string(),
            skills: vec![
                Skill {
                    name: "text-transform".to_string(),
                    description: "Transform text to uppercase, lowercase, reverse, title case, \
                                  snake case, or count words/characters."
                        .to_string(),
                    examples: vec![
                        "Transform 'hello world' to uppercase".to_string(),
                        "Reverse the text 'abcdef'".to_string(),
                        "Count the words in this paragraph".to_string(),
                    ],
                },
            ],
            tools: vec![ToolDefinition {
                name: "transform_text".to_string(),
                description: "Transform text using a specified operation.".to_string(),
                parameters_schema: r#"{"type":"object","properties":{"text":{"type":"string"},"operation":{"type":"string","enum":["uppercase","lowercase","reverse","word_count","char_count","title_case","snake_case"]}},"required":["text","operation"]}"#.to_string(),
            }],
        }
    }
}

export_module!(EchoAgent);
