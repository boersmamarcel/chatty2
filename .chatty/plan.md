# AGE-427: Remove ProviderType serde aliases; settings load errors name file and value

## Acceptance criteria
- ProviderType deserializes only open_router, ollama, azure_openai; open_ai, open_a_i, anthropic, gemini, mistral are errors.
- Loading providers.json with "provider_type":"anthropic" fails with an error containing the file path and `anthropic`; no fallback.
- A models.json list repo that does not parse fails with an error containing its path.
- Settings tests, integration test model_config_missing_optional_fields_use_defaults, docs drift check pass; clippy -D warnings and cargo fmt --all --check pass.

## Steps
1. [x] Remove aliases in providers_store.rs (+ test replaced), models/mod.rs assertion, integration.rs fixture, gen-docs-reference.sh sentences; drift check passes. (commit: 061eec92)
2. [ ] secret_json_repository.rs / generic_json_repository.rs: path-prefixed SerializationError messages on load.
3. [ ] Add tests: a_removed_provider_type_is_a_load_error_naming_the_file_and_value; unparseable_list_file_is_an_error_naming_the_path.
4. [ ] Run checks: settings tests, integration test, clippy, fmt, drift check, grep alias.
