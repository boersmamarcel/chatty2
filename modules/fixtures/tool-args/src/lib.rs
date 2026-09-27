//! Fixture `tool-args`: `invoke_tool` returns its raw `args` string unchanged.
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
        reply(last_user(&req).to_string())
    }
    fn invoke_tool(&self, _name: String, args: String) -> Result<String, String> {
        Ok(args)
    }
    fn list_tools(&self) -> Vec<ToolDefinition> {
        let (name, description) = ("echo_args".into(), "Returns its raw arguments.".into());
        let parameters_schema =
            r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn get_agent_card(&self) -> AgentCard {
        card("tool-args", self.list_tools())
    }
}

export_module!(Fixture);
