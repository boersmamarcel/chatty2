//! Fixture `wit-0.1`: a component built against `chatty:module@0.1.0`, which
//! the host (`chatty:plugin@0.4.0` only) must refuse to load with the
//! rebuild message.
wit_bindgen::generate!({ world: "module", path: "wit" });

struct Fixture;

impl exports::chatty::module::agent::Guest for Fixture {
    fn get_agent_card() -> String {
        "wit-0.1".to_string()
    }
}

export!(Fixture);
