//! The install record at load time (PL-H5a, AGE-703): a module copied in by
//! hand, with no `.chatty-install.json`, still loads, as `TrustLevel::Local`.
//! The tampered-install half lives with the installer
//! (`chatty-core` `install::tests::tampered_module_refused_at_load`).

use std::sync::Arc;

use chatty_module_registry::{INSTALL_RECORD_FILE, ModuleRegistry, TrustLevel};
use chatty_wasm_runtime::test_support::fixture_path;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};

struct NoopLlm;

impl LlmProvider for NoopLlm {
    fn complete(
        &self,
        _: &str,
        _: Vec<Message>,
        _: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop".to_string())
    }
}

#[test]
fn local_module_loads_as_local() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("hand-copied");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture_path("tool-args"), dir.join("mod.wasm")).unwrap();
    std::fs::write(
        dir.join("module.toml"),
        "[module]\nname = \"hand-copied\"\nversion = \"0.1.0\"\nwasm = \"mod.wasm\"\n",
    )
    .unwrap();
    assert!(!dir.join(INSTALL_RECORD_FILE).exists());

    let mut registry =
        ModuleRegistry::new(Arc::new(NoopLlm), ResourceLimits::default()).expect("registry");
    let report = registry.scan_directory(root.path()).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.loaded_names(), vec!["hand-copied"]);
    assert_eq!(registry.trust_level("hand-copied"), Some(TrustLevel::Local));
}
