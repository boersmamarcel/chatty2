//! Fixture `stateful`: `chat` counts its calls in a static and returns the count.
use chatty_module_sdk::{export_module, ToolDefinition};
use chatty_module_sdk::{AgentCard, ChatRequest, ChatResponse, ModuleExports, Role};
use std::sync::atomic::{AtomicU64, Ordering};

static CALLS: AtomicU64 = AtomicU64::new(0);

fn card(name: &str, tools: Vec<ToolDefinition>) -> AgentCard {
    let (name, display_name) = (name.to_string(), name.to_string());
    let (description, version) = ("Test fixture.".to_string(), "0.1.0".to_string());
    let skills = vec![];
    AgentCard { name, display_name, description, version, skills, tools }
}

#[derive(Default)]
struct Fixture;

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
        let _ = last_user(&req);
        reply((CALLS.fetch_add(1, Ordering::SeqCst) + 1).to_string())
    }
    fn invoke_tool(&self, name: String, _args: String) -> Result<String, String> {
        Err(format!("unknown tool: {name}"))
    }
    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    fn get_agent_card(&self) -> AgentCard {
        card("stateful", vec![])
    }
}

export_module!(Fixture);
