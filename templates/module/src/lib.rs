use chatty_module_sdk::{
    export_module, AgentCard, ChatRequest, ChatResponse, ModuleExports, Skill, ToolDefinition,
};

/// Replace `Agent` with your own agent type name.
#[derive(Default)]
pub struct Agent;

impl ModuleExports for Agent {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        let msg = req
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or("");

        Ok(ChatResponse {
            content: format!("You said: {msg}"),
            tool_calls: vec![],
            usage: None,
        })
    }

    fn invoke_tool(&self, name: String, args: String) -> Result<String, String> {
        Err(format!("unknown tool: {name} (args: {args})"))
    }

    fn list_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }

    fn get_agent_card(&self) -> AgentCard {
        AgentCard {
            name: "{{project-name}}".to_string(),
            display_name: "{{project-name}}".to_string(),
            description: "{{description}}".to_string(),
            version: "0.1.0".to_string(),
            skills: vec![Skill {
                name: "default".to_string(),
                description: "Default skill".to_string(),
                examples: vec!["Say hello".to_string()],
            }],
            tools: vec![],
        }
    }
}

export_module!(Agent);
