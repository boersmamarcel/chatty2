use chatty_module_registry::ModuleRegistry;
use chatty_wasm_runtime::{LlmProvider, Message, CompletionResponse, ResourceLimits};
use std::sync::Arc;
use std::path::PathBuf;

struct Noop;
impl LlmProvider for Noop {
    fn complete(&self, _: &str, _: Vec<Message>, _: Option<String>) -> Result<CompletionResponse, String> {
        Err("noop".into())
    }
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("module dir"));
    let provider: Arc<dyn LlmProvider> = Arc::new(Noop);
    let mut reg = ModuleRegistry::new(provider, ResourceLimits::default()).expect("init");
    match reg.load(&dir) {
        Ok(name) => println!("OK loaded: {name}"),
        Err(e) => println!("ERR: {e:#?}"),
    }
}
