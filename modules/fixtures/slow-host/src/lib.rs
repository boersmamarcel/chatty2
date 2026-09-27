//! Fixture `slow-host`: `chat` calls the host's `llm::complete`, so a slow provider stalls it in host time.
use chatty_module_sdk::{export_module, ToolDefinition};
use chatty_module_sdk::{AgentCard, ChatRequest, ChatResponse, ModuleExports, Role};

fn card(name: &str, tools: Vec<ToolDefinition>) -> AgentCard {
    let (name, display_name) = (name.to_string(), name.to_string());
    let (description, version) = ("Test fixture.".to_string(), "0.1.0".to_string());
    let skills = vec![];
    AgentCard { name, display_name, description, version, skills, tools }
}

#[derive(Default)]
struct Fixture;

#[allow(dead_code)]
fn last_user(req: &ChatRequest) -> &str {
    let user = req.messages.iter().rfind(|m| m.role == Role::User);
    user.map_or("", |m| m.content.as_str())
}

#[allow(dead_code)]
fn reply(content: String) -> Result<ChatResponse, String> {
    let (tool_calls, usage) = (vec![], None);
    Ok(ChatResponse { content, tool_calls, usage })
}

impl ModuleExports for Fixture {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        let resp = chatty_module_sdk::llm::complete("", &req.messages, None)?;
        reply(resp.content)
    }
    fn invoke_tool(&self, name: String, _args: String) -> Result<String, String> {
        Err(format!("unknown tool: {name}"))
    }
    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    fn get_agent_card(&self) -> AgentCard {
        card("slow-host", vec![])
    }
}

export_module!(Fixture);
