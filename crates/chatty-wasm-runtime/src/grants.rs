//! Capability grants (PL-U4, AGE-619): a plugin *requests* capabilities in
//! its `metadata`, the host *grants* a subset, and the linker links only
//! what was granted.
//!
//! Every host import is its own WIT interface, one per [`Capability`]. A
//! module is linked against the real host implementation of a capability
//! only when it was granted; an ungranted import that the component still
//! imports is satisfied by a stub that refuses with
//! `capability <x> not granted to this agent` ([`NotGranted`]), so
//! instantiation succeeds and the refusal reaches the model through the
//! tool result. `logging` is always linked. A module served with no agent
//! spec also gets `config` by default; `llm`, `file` and `billing` need the
//! user's grant ([`Grants::Specless`]).
//!
//! A grant the plugin did not request is refused at load
//! ([`UnrequestedGrant`]): the grant list is checked against the
//! `requested-capabilities` the plugin's `metadata` returns, which is read
//! from an instance with nothing granted.

use std::fmt;

use tracing::warn;
use wasmtime::component::{HasData, HasSelf, Linker};

use crate::bindings::chatty::plugin::billing::SessionInfo;
use crate::bindings::chatty::plugin::types::{CompletionResponse, Message};
use crate::bindings::chatty::plugin::{billing, config, file, llm, logging, types};
use crate::bindings::exports::chatty::plugin::plugin::Capability;
use crate::host::ModuleState;

impl Capability {
    /// Every capability, in WIT order.
    pub const ALL: [Capability; 5] = [
        Capability::Llm,
        Capability::Config,
        Capability::Logging,
        Capability::File,
        Capability::Billing,
    ];

    /// The capability's WIT name (`llm`, `config`, `logging`, `file`,
    /// `billing`).
    pub fn name(self) -> &'static str {
        match self {
            Capability::Llm => "llm",
            Capability::Config => "config",
            Capability::Logging => "logging",
            Capability::File => "file",
            Capability::Billing => "billing",
        }
    }

    /// The capability a WIT name names.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a module is linked against, besides `logging` (always linked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grants {
    /// Exactly these capabilities, each of which the plugin must request.
    /// The default is none.
    Only(Vec<Capability>),
    /// A module served on its own (the gateway's registry), with no agent
    /// spec to grant from (SEC-11, AGE-815): it gets
    /// [`SPECLESS_DEFAULTS`] if it requests them, plus whatever the user
    /// approved in Settings (`approved`) that it also requests. A stale
    /// approval for something the module no longer requests links nothing.
    Specless {
        /// The capabilities the user granted this module in Settings.
        approved: Vec<Capability>,
    },
}

/// What a module served with no agent spec gets without a grant from the
/// user, besides `logging`: `llm`, `file` and `billing` need one.
pub const SPECLESS_DEFAULTS: [Capability; 1] = [Capability::Config];

impl Default for Grants {
    fn default() -> Self {
        Self::Only(Vec::new())
    }
}

/// A host import refused because its capability was not granted. The
/// `Err` a result-returning import hands the guest is this error's text; an
/// import with no error channel (`config::get`) traps with it, and the call
/// fails with [`CallError::NotGranted`](crate::CallError::NotGranted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotGranted(pub Capability);

impl fmt::Display for NotGranted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "capability {} not granted to this agent", self.0)
    }
}

impl std::error::Error for NotGranted {}

/// A load refused because a grant names a capability the plugin does not
/// request. Reach it with `err.downcast_ref::<UnrequestedGrant>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrequestedGrant {
    /// The plugin's name.
    pub module: String,
    /// The granted capabilities it did not request.
    pub unrequested: Vec<Capability>,
    /// What it does request.
    pub requested: Vec<Capability>,
}

impl fmt::Display for UnrequestedGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = |caps: &[Capability]| {
            caps.iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let requested = if self.requested.is_empty() {
            "nothing".to_string()
        } else {
            names(&self.requested)
        };
        write!(
            f,
            "plugin `{}` is granted {}, which it does not request (it requests {requested}); \
             grant only capabilities the plugin requests",
            self.module,
            names(&self.unrequested)
        )
    }
}

impl std::error::Error for UnrequestedGrant {}

/// The capabilities a module is linked against: `grants` resolved against
/// what it `requested`, always with `logging`, in WIT order. A grant it did
/// not request is an [`UnrequestedGrant`] (`logging` is always allowed).
pub(crate) fn resolve(
    module: &str,
    grants: &Grants,
    requested: &[Capability],
) -> Result<Vec<Capability>, UnrequestedGrant> {
    let specless: Vec<Capability>;
    let granted: &[Capability] = match grants {
        Grants::Only(granted) => granted,
        Grants::Specless { approved } => {
            specless = requested
                .iter()
                .copied()
                .filter(|c| SPECLESS_DEFAULTS.contains(c) || approved.contains(c))
                .collect();
            &specless
        }
    };
    let unrequested: Vec<Capability> = Capability::ALL
        .into_iter()
        .filter(|c| *c != Capability::Logging && granted.contains(c) && !requested.contains(c))
        .collect();
    if !unrequested.is_empty() {
        return Err(UnrequestedGrant {
            module: module.to_string(),
            unrequested,
            requested: requested.to_vec(),
        });
    }
    Ok(Capability::ALL
        .into_iter()
        .filter(|c| *c == Capability::Logging || granted.contains(c))
        .collect())
}

/// Link the plugin world's imports into `linker`: the real host side of
/// each capability in `granted`, a refusing stub for the others, and
/// `logging` always.
pub(crate) fn add_to_linker(
    linker: &mut Linker<ModuleState>,
    granted: &[Capability],
) -> anyhow::Result<()> {
    let real = |c: Capability| granted.contains(&c);
    types::add_to_linker::<_, HasSelf<ModuleState>>(linker, |s| s)?;
    logging::add_to_linker::<_, HasSelf<ModuleState>>(linker, |s| s)?;
    if real(Capability::Llm) {
        llm::add_to_linker::<_, HasSelf<ModuleState>>(linker, |s| s)?;
    } else {
        llm::add_to_linker::<_, Refusing>(linker, refused)?;
    }
    if real(Capability::Config) {
        config::add_to_linker::<_, HasSelf<ModuleState>>(linker, |s| s)?;
    } else {
        config::add_to_linker::<_, Refusing>(linker, refused)?;
    }
    if real(Capability::File) {
        file::add_to_linker::<_, HasSelf<ModuleState>>(linker, |s| s)?;
    } else {
        file::add_to_linker::<_, Refusing>(linker, refused)?;
    }
    if real(Capability::Billing) {
        billing::add_to_linker::<_, HasSelf<ModuleState>>(linker, |s| s)?;
    } else {
        billing::add_to_linker::<_, Refusing>(linker, refused)?;
    }
    Ok(())
}

fn refused(state: &mut ModuleState) -> Refused<'_> {
    Refused(state)
}

/// Links the [`Refused`] host side for a capability.
struct Refusing;

impl HasData for Refusing {
    type Data<'a> = Refused<'a>;
}

/// The host side of every ungranted capability: each call is refused.
struct Refused<'a>(&'a mut ModuleState);

impl Refused<'_> {
    fn refuse(&self, capability: Capability) -> NotGranted {
        warn!(
            module = %self.0.manifest.name,
            capability = capability.name(),
            "plugin called a capability it was not granted"
        );
        NotGranted(capability)
    }
}

impl llm::Host for Refused<'_> {
    fn complete(
        &mut self,
        _model: String,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err(self.refuse(Capability::Llm).to_string())
    }
}

impl config::Host for Refused<'_> {
    /// `config::get` has no error channel, so the refusal is a trap: the
    /// call fails with the refusal as its reason.
    fn get(&mut self, _key: String) -> wasmtime::Result<Option<String>> {
        Err(wasmtime::Error::new(self.refuse(Capability::Config)))
    }
}

impl file::Host for Refused<'_> {
    fn read_bytes(&mut self, _path: String) -> Result<Vec<u8>, String> {
        Err(self.refuse(Capability::File).to_string())
    }
}

impl billing::Host for Refused<'_> {
    fn acquire_session(&mut self, _estimated_tokens: i64) -> Result<SessionInfo, String> {
        Err(self.refuse(Capability::Billing).to_string())
    }

    fn report_usage(&mut self, _input_tokens: i64, _output_tokens: i64) -> Result<(), String> {
        Err(self.refuse(Capability::Billing).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for c in Capability::ALL {
            assert_eq!(Capability::from_name(c.name()), Some(c));
        }
        assert_eq!(Capability::from_name("http"), None);
    }

    #[test]
    fn nothing_granted_links_only_logging() {
        let linked = resolve("m", &Grants::default(), &[Capability::Llm]).unwrap();
        assert_eq!(linked, [Capability::Logging]);
    }

    #[test]
    fn specless_links_config_and_what_the_user_approved() {
        let requested = [Capability::Billing, Capability::Config, Capability::Llm];
        let none = Grants::Specless { approved: vec![] };
        assert_eq!(
            resolve("m", &none, &requested).unwrap(),
            [Capability::Config, Capability::Logging]
        );
        let llm = Grants::Specless {
            approved: vec![Capability::Llm, Capability::File],
        };
        // `file` was approved but is not requested: it links nothing.
        assert_eq!(
            resolve("m", &llm, &requested).unwrap(),
            [Capability::Llm, Capability::Config, Capability::Logging]
        );
    }

    #[test]
    fn logging_may_always_be_granted() {
        let grants = Grants::Only(vec![Capability::Logging]);
        assert_eq!(resolve("m", &grants, &[]).unwrap(), [Capability::Logging]);
    }

    #[test]
    fn an_unrequested_grant_is_refused_by_name() {
        let grants = Grants::Only(vec![Capability::Llm, Capability::File]);
        let err = resolve("echo", &grants, &[Capability::Llm]).unwrap_err();
        assert_eq!(err.unrequested, [Capability::File]);
        assert_eq!(
            err.to_string(),
            "plugin `echo` is granted `file`, which it does not request (it requests `llm`); \
             grant only capabilities the plugin requests"
        );
        let err = resolve("echo", &Grants::Only(vec![Capability::File]), &[]).unwrap_err();
        assert!(err.to_string().contains("(it requests nothing)"), "{err}");
    }

    #[test]
    fn the_refusal_names_the_capability() {
        assert_eq!(
            NotGranted(Capability::Config).to_string(),
            "capability config not granted to this agent"
        );
    }
}
