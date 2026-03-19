//! External function bridge between Monty-executed Python code and
//! chatty2's rig.rs tools / MCP servers.
//!
//! # Overview
//!
//! When the LLM writes Python code that calls `get_weather("Amsterdam")`,
//! `MontySandbox` encounters an *external function call*.  Instead of
//! failing, it pauses execution and hands control back to this bridge,
//! which dispatches to the real chatty2 tool and resumes the script with
//! the result.
//!
//! This enables the "code mode" pattern:
//!
//! ```text
//! LLM generates one Python script → Monty executes it →
//! each tool call pauses execution → bridge dispatches to real tool →
//! execution resumes with the tool's output
//! ```
//!
//! This is more efficient than sequential JSON tool calls because a single
//! LLM round-trip produces the entire execution plan.
//!
//! # Current status (Phase 1 → 2 boundary)
//!
//! `ToolBridge` is defined here with its full interface so that callers
//! can register dispatchers and obtain function-name lists.  The actual
//! snapshot / resume loop that drives the interleaved execution will be
//! wired in Phase 2 once the upstream Monty VM API is stable.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;

// ─── MontyValue ───────────────────────────────────────────────────────────────

/// A Rust-native value that can be passed into or returned from a Monty
/// external function call.
///
/// This mirrors the `MontyObject` enum that the upstream Monty crate
/// exposes, but is defined here to keep the bridge decoupled from a
/// specific Monty crate version.  A conversion layer will be added in
/// Phase 2 once the Monty crate stabilises on crates.io.
#[derive(Debug, Clone, PartialEq)]
pub enum MontyValue {
    /// `None` / null
    None,
    /// Boolean
    Bool(bool),
    /// Integer (Python's arbitrary-precision int is capped to i64 here)
    Int(i64),
    /// Floating-point number
    Float(f64),
    /// UTF-8 string
    Str(String),
    /// Ordered sequence of values
    List(Vec<MontyValue>),
    /// Key-value mapping (string keys only for JSON compatibility)
    Dict(HashMap<String, MontyValue>),
}

impl MontyValue {
    /// Convert to a JSON-serialisable `serde_json::Value` for tool dispatch.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            MontyValue::None => serde_json::Value::Null,
            MontyValue::Bool(b) => serde_json::Value::Bool(*b),
            MontyValue::Int(i) => serde_json::Value::Number((*i).into()),
            MontyValue::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            MontyValue::Str(s) => serde_json::Value::String(s.clone()),
            MontyValue::List(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_json()).collect())
            }
            MontyValue::Dict(map) => serde_json::Value::Object(
                map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect(),
            ),
        }
    }

    /// Construct a `MontyValue` from a `serde_json::Value` returned by a tool.
    pub fn from_json(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => MontyValue::None,
            serde_json::Value::Bool(b) => MontyValue::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    MontyValue::Int(i)
                } else {
                    MontyValue::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => MontyValue::Str(s),
            serde_json::Value::Array(arr) => {
                MontyValue::List(arr.into_iter().map(MontyValue::from_json).collect())
            }
            serde_json::Value::Object(map) => MontyValue::Dict(
                map.into_iter()
                    .map(|(k, v)| (k, MontyValue::from_json(v)))
                    .collect(),
            ),
        }
    }

    /// Return a Python-repr–like string for display / debugging.
    pub fn to_repr(&self) -> String {
        match self {
            MontyValue::None => "None".to_string(),
            MontyValue::Bool(true) => "True".to_string(),
            MontyValue::Bool(false) => "False".to_string(),
            MontyValue::Int(i) => i.to_string(),
            MontyValue::Float(f) => format!("{f}"),
            MontyValue::Str(s) => format!("{s:?}"),
            MontyValue::List(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.to_repr()).collect();
                format!("[{}]", inner.join(", "))
            }
            MontyValue::Dict(map) => {
                let inner: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{k:?}: {}", v.to_repr()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }
}

// ─── ToolDispatcher ───────────────────────────────────────────────────────────

/// A single callable that handles one external function invocation from
/// Monty-executed Python code.
///
/// Implementors receive the function name and positional arguments as
/// [`MontyValue`]s and must return a [`MontyValue`] to resume execution.
///
/// # Example
///
/// ```rust,ignore
/// struct WeatherDispatcher;
///
/// #[async_trait]
/// impl ToolDispatcher for WeatherDispatcher {
///     fn name(&self) -> &str { "get_weather" }
///
///     fn python_stub(&self) -> String {
///         "def get_weather(city: str) -> str: ...".to_string()
///     }
///
///     async fn dispatch(
///         &self,
///         _name: &str,
///         args: Vec<MontyValue>,
///     ) -> Result<MontyValue> {
///         let city = match args.into_iter().next() {
///             Some(MontyValue::Str(s)) => s,
///             _ => anyhow::bail!("expected a string city argument"),
///         };
///         let weather = fetch_weather_api(&city).await?;
///         Ok(MontyValue::Str(weather))
///     }
/// }
/// ```
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// The Python function name as it will appear in the LLM-generated code.
    fn name(&self) -> &str;

    /// A Python type stub line (PEP 484 style) that is injected into the
    /// system prompt so the LLM knows the function's signature and return
    /// type.  Example: `"def get_weather(city: str) -> str: ..."`.
    fn python_stub(&self) -> String;

    /// Dispatch an external function call from Monty to the real tool.
    ///
    /// `function_name` is the exact name the Python code used (equals
    /// [`Self::name`]).  `args` are the positional arguments in call order.
    async fn dispatch(&self, function_name: &str, args: Vec<MontyValue>) -> Result<MontyValue>;
}

// ─── ToolBridge ───────────────────────────────────────────────────────────────

/// Registry of chatty2 tools available as external functions in Monty-
/// executed Python code.
///
/// Call [`register`] for each tool you want the LLM to be able to call,
/// then pass [`function_names`] to `MontyRun::new` (Phase 2) and inject
/// [`type_stubs`] into the system prompt.
///
/// The actual snapshot → dispatch → resume loop will be implemented in
/// Phase 2 on top of this bridge.
pub struct ToolBridge {
    dispatchers: HashMap<String, Box<dyn ToolDispatcher>>,
}

impl ToolBridge {
    /// Create an empty bridge.
    pub fn new() -> Self {
        Self {
            dispatchers: HashMap::new(),
        }
    }

    /// Register a tool dispatcher.  The function name is taken from
    /// [`ToolDispatcher::name`].
    pub fn register(&mut self, dispatcher: Box<dyn ToolDispatcher>) {
        self.dispatchers
            .insert(dispatcher.name().to_string(), dispatcher);
    }

    /// Returns the list of external function names to pass to Monty's
    /// `MontyRun::new(…, external_fn_names)` parameter (Phase 2).
    pub fn function_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.dispatchers.keys().cloned().collect();
        names.sort(); // deterministic order for system-prompt generation
        names
    }

    /// Generate Python type stubs for all registered tools.
    ///
    /// The stubs are injected into the system prompt so the LLM knows
    /// which functions are available and what their signatures are.
    ///
    /// Example output:
    /// ```python
    /// def get_weather(city: str) -> str: ...
    /// def search_web(query: str) -> list[str]: ...
    /// ```
    pub fn type_stubs(&self) -> String {
        let mut stubs: Vec<String> = self.dispatchers.values().map(|d| d.python_stub()).collect();
        stubs.sort(); // deterministic
        stubs.join("\n")
    }

    /// Dispatch an external function call from Monty.
    ///
    /// Returns an error if no dispatcher is registered for `function_name`.
    pub async fn dispatch(&self, function_name: &str, args: Vec<MontyValue>) -> Result<MontyValue> {
        let dispatcher = self.dispatchers.get(function_name).ok_or_else(|| {
            anyhow::anyhow!(
                "No tool registered for external function '{function_name}'. \
                 Available: {:?}",
                self.function_names()
            )
        })?;

        dispatcher.dispatch(function_name, args).await
    }

    /// Returns `true` if at least one dispatcher is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dispatchers.is_empty()
    }

    /// Returns the number of registered dispatchers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.dispatchers.len()
    }
}

impl Default for ToolBridge {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MontyValue ────────────────────────────────────────────────────────────

    #[test]
    fn monty_value_round_trips_through_json() {
        let original = MontyValue::Dict({
            let mut m = HashMap::new();
            m.insert("x".to_string(), MontyValue::Int(42));
            m.insert("y".to_string(), MontyValue::Str("hello".to_string()));
            m.insert("z".to_string(), MontyValue::Bool(true));
            m
        });

        let json = original.to_json();
        let restored = MontyValue::from_json(json);

        // Spot-check individual fields (HashMap ordering is not guaranteed).
        if let MontyValue::Dict(map) = restored {
            assert_eq!(map["x"], MontyValue::Int(42));
            assert_eq!(map["y"], MontyValue::Str("hello".to_string()));
            assert_eq!(map["z"], MontyValue::Bool(true));
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn monty_value_repr() {
        assert_eq!(MontyValue::None.to_repr(), "None");
        assert_eq!(MontyValue::Bool(true).to_repr(), "True");
        assert_eq!(MontyValue::Int(7).to_repr(), "7");
        assert_eq!(MontyValue::Str("hi".to_string()).to_repr(), r#""hi""#);
    }

    #[test]
    fn monty_value_list_round_trip() {
        let list = MontyValue::List(vec![
            MontyValue::Int(1),
            MontyValue::Float(2.5),
            MontyValue::None,
        ]);
        let json = list.to_json();
        let restored = MontyValue::from_json(json);
        assert_eq!(
            restored,
            MontyValue::List(vec![
                MontyValue::Int(1),
                MontyValue::Float(2.5),
                MontyValue::None,
            ])
        );
    }

    // ── ToolBridge ────────────────────────────────────────────────────────────

    struct EchoDispatcher;

    #[async_trait]
    impl ToolDispatcher for EchoDispatcher {
        fn name(&self) -> &str {
            "echo"
        }

        fn python_stub(&self) -> String {
            "def echo(value: str) -> str: ...".to_string()
        }

        async fn dispatch(&self, _name: &str, args: Vec<MontyValue>) -> Result<MontyValue> {
            Ok(args.into_iter().next().unwrap_or(MontyValue::None))
        }
    }

    #[tokio::test]
    async fn bridge_dispatches_to_registered_tool() {
        let mut bridge = ToolBridge::new();
        bridge.register(Box::new(EchoDispatcher));

        let result = bridge
            .dispatch("echo", vec![MontyValue::Str("ping".to_string())])
            .await
            .unwrap();

        assert_eq!(result, MontyValue::Str("ping".to_string()));
    }

    #[tokio::test]
    async fn bridge_returns_error_for_unknown_function() {
        let bridge = ToolBridge::new();
        let result = bridge.dispatch("unknown", vec![]).await;
        assert!(result.is_err());
    }

    #[test]
    fn bridge_type_stubs_are_sorted() {
        let mut bridge = ToolBridge::new();

        struct StubDispatcher(&'static str, &'static str);
        #[async_trait]
        impl ToolDispatcher for StubDispatcher {
            fn name(&self) -> &str {
                self.0
            }
            fn python_stub(&self) -> String {
                self.1.to_string()
            }
            async fn dispatch(&self, _: &str, _: Vec<MontyValue>) -> Result<MontyValue> {
                Ok(MontyValue::None)
            }
        }

        bridge.register(Box::new(StubDispatcher("zzz", "def zzz(): ...")));
        bridge.register(Box::new(StubDispatcher("aaa", "def aaa(): ...")));

        let stubs = bridge.type_stubs();
        let first_line = stubs.lines().next().unwrap();
        assert_eq!(first_line, "def aaa(): ...");
    }

    #[test]
    fn bridge_function_names_are_sorted() {
        let mut bridge = ToolBridge::new();

        struct SimpleDispatcher(&'static str);
        #[async_trait]
        impl ToolDispatcher for SimpleDispatcher {
            fn name(&self) -> &str {
                self.0
            }
            fn python_stub(&self) -> String {
                String::new()
            }
            async fn dispatch(&self, _: &str, _: Vec<MontyValue>) -> Result<MontyValue> {
                Ok(MontyValue::None)
            }
        }

        bridge.register(Box::new(SimpleDispatcher("zzz")));
        bridge.register(Box::new(SimpleDispatcher("aaa")));

        let names = bridge.function_names();
        assert_eq!(names[0], "aaa");
        assert_eq!(names[1], "zzz");
    }
}
