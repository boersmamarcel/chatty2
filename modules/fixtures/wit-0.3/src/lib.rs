//! Fixture `wit-0.3`: a component built against `chatty:plugin@0.3.0`, the
//! world before `billing` left it (MK-2), which the host (`chatty:plugin@0.4.0`
//! only) must refuse to load with the rebuild message.
wit_bindgen::generate!({ world: "plugin-world", path: "wit" });

struct Fixture;

impl exports::chatty::plugin::plugin::Guest for Fixture {
    fn list_tools() -> Vec<String> {
        Vec::new()
    }
}

export!(Fixture);
