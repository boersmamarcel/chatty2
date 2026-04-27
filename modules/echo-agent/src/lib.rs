//! Echo agent — reference chatty WASM module.
//!
//! Demonstrates every chatty SDK feature:
//!
//! * **chat** — echoes the last user message prefixed with `"Echo: "`.
//!   If the message contains `"use llm"`, calls [`chatty_module_sdk::llm::complete`]
//!   to show the host LLM import callback.
//! * **tools** — `echo`, `reverse`, `count_words`
//! * **agent card** — name `"echo-agent"`, skill `"echoing"`
//! * **logging** — emits info/debug/warn log lines via the host logging import
//!
//! See `README.md` for the module author quickstart.

use chatty_module_sdk::{
    export_module, AgentCard, ChatRequest, ChatResponse, ModuleExports, Role, Skill, ToolDefinition,
};

// ---------------------------------------------------------------------------
// EchoAgent struct
// ---------------------------------------------------------------------------

/// The echo agent module implementation.
#[derive(Default)]
pub struct EchoAgent;

impl ModuleExports for EchoAgent {
    // -----------------------------------------------------------------------
    // chat
    // -----------------------------------------------------------------------

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        chatty_module_sdk::log::info("echo-agent: handling chat request");

        // Find the last user message.
        let last_user_msg = req
            .messages
            .iter()
            .rfind(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("");

        // If the user asks to "use llm", delegate to the host LLM import.
        let content = if last_user_msg.contains("use llm") {
            chatty_module_sdk::log::debug("echo-agent: delegating to host LLM");
            match chatty_module_sdk::llm::complete("", &req.messages, None) {
                Ok(resp) => resp.content,
                Err(e) => {
                    chatty_module_sdk::log::warn(&format!("echo-agent: LLM error: {e}"));
                    format!("LLM error: {e}")
                }
            }
        } else {
            format!("Echo: {last_user_msg}")
        };

        Ok(ChatResponse {
            content,
            tool_calls: vec![],
            usage: None,
        })
    }

    // -----------------------------------------------------------------------
    // invoke_tool
    // -----------------------------------------------------------------------

    fn invoke_tool(&self, name: String, args: String) -> Result<String, String> {
        chatty_module_sdk::log::info(&format!("echo-agent: invoke_tool name={name}"));

        match name.as_str() {
            // Return the input string unchanged.
            "echo" => Ok(args),
            // Return the input string with characters reversed.
            "reverse" => Ok(args.chars().rev().collect()),
            // Return the number of whitespace-separated words.
            "count_words" => Ok(args.split_whitespace().count().to_string()),
            _ => {
                chatty_module_sdk::log::error(&format!("echo-agent: unknown tool: {name}"));
                Err(format!("unknown tool: {name}"))
            }
        }
    }

    // -----------------------------------------------------------------------
    // list_tools
    // -----------------------------------------------------------------------

    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "echo".to_string(),
                description: "Returns the input string unchanged.".to_string(),
                parameters_schema: concat!(
                    r#"{"type":"object","properties":{"input":{"type":"string","#,
                    r#""description":"String to echo"}},"required":["input"]}"#
                )
                .to_string(),
            },
            ToolDefinition {
                name: "reverse".to_string(),
                description: "Returns the input string with characters in reverse order."
                    .to_string(),
                parameters_schema: concat!(
                    r#"{"type":"object","properties":{"input":{"type":"string","#,
                    r#""description":"String to reverse"}},"required":["input"]}"#
                )
                .to_string(),
            },
            ToolDefinition {
                name: "count_words".to_string(),
                description:
                    "Returns the number of whitespace-separated words in the input string."
                        .to_string(),
                parameters_schema: concat!(
                    r#"{"type":"object","properties":{"input":{"type":"string","#,
                    r#""description":"String to count words in"}},"required":["input"]}"#
                )
                .to_string(),
            },
        ]
    }

    // -----------------------------------------------------------------------
    // get_agent_card
    // -----------------------------------------------------------------------

    fn get_agent_card(&self) -> AgentCard {
        AgentCard {
            name: "echo-agent".to_string(),
            display_name: "Echo Agent".to_string(),
            description: "Reference echo agent demonstrating all chatty SDK features.".to_string(),
            version: "0.1.0".to_string(),
            skills: vec![Skill {
                name: "echoing".to_string(),
                description: "Echoes user messages back; optionally calls the host LLM."
                    .to_string(),
                examples: vec![
                    "Say hello".to_string(),
                    "use llm to answer: what is 2+2?".to_string(),
                ],
            }],
            tools: vec![],
        }
    }
}

// Wire the trait implementation to the WIT guest exports.
export_module!(EchoAgent);
