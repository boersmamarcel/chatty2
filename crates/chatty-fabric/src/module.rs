//! Paid plugins on the fabric (ADR-0024 §§ 1–2, 7; MK-2).
//!
//! A paid plugin's WASM never runs in a worker, on a desktop or in the
//! local gateway: it runs only in hive's module runtime, reached by a
//! `module.call` the worker sends its broker. So the worker builds the
//! plugin's tools from the published version's lockfile manifest — its tool
//! list and schemas ([`PaidPluginManifest`]) — never from the bytes, and
//! each call is one [`ModuleCallParams`] answered by one
//! [`ModuleCallOutcome`].
//!
//! Every money condition (acceptance, caps, limits, review, suspension,
//! funding) is the ledger's, and a refused one comes back as a typed
//! [`CallError::NeedsAcceptance`](crate::CallError::NeedsAcceptance) or
//! [`CallError::FeeRefused`](crate::CallError::FeeRefused). Neither is the
//! calling model's to work around: a node's run that hits one fails with it
//! ([`find_money_refusal`]), and every caller up the tree passes it on
//! unchanged, so the root shows it to the user.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a fee is charged for (ADR-0024 § 1): a plugin per call, a team per
/// run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Plugin,
    Team,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Team => "team",
        }
    }
}

/// One published version: a plugin or a team, by name and exact version
/// (ADR-0024 § 5's `(kind, name, version)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemRef {
    pub kind: ItemKind,
    pub name: String,
    pub version: String,
}

impl std::fmt::Display for ItemRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}@{}", self.kind.as_str(), self.name, self.version)
    }
}

/// Why the ledger refused a fee (ADR-0024 § 7, L8): a closed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeeRefusalReason {
    /// The user's monthly item cap for it is spent.
    Cap,
    /// The calling run's per-run limit for it is spent.
    Limit,
    /// The (payer, publisher) monthly limit before the publisher has a
    /// clean history.
    Velocity,
    /// No fee-eligible credit for the fee and its VAT.
    Funding,
    Suspended,
    /// A listed version with no review-passed row.
    Review,
    /// An unlisted version, and the payer is not its publisher.
    Unlisted,
    /// A `module.call` rate or concurrency quota.
    Quota,
}

impl FeeRefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cap => "cap",
            Self::Limit => "limit",
            Self::Velocity => "velocity",
            Self::Funding => "funding",
            Self::Suspended => "suspended",
            Self::Review => "review",
            Self::Unlisted => "unlisted",
            Self::Quota => "quota",
        }
    }
}

impl std::fmt::Display for FeeRefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The typed prefix of a [`CallError::NeedsAcceptance`](crate::CallError::NeedsAcceptance)'s text.
pub const NEEDS_ACCEPTANCE: &str = "needs_acceptance";

/// The typed prefix of a [`CallError::FeeRefused`](crate::CallError::FeeRefused)'s text.
pub const FEE_REFUSED: &str = "fee_refused";

/// The money refusal `text` carries, wherever in it — a tool error, a
/// sub-leader's failed task — as the one line that names it: from its
/// typed prefix to the end of that line. A caller that wraps the error
/// still carries it, and passes it on unchanged.
pub fn find_money_refusal(text: &str) -> Option<&str> {
    let start = [NEEDS_ACCEPTANCE, FEE_REFUSED]
        .iter()
        .filter_map(|prefix| text.find(&format!("{prefix}: ")))
        .min()?;
    let rest = &text[start..];
    Some(rest.split('\n').next().unwrap_or(rest).trim_end())
}

/// A plugin version a published spec's lockfile pins: the module, its
/// exact version and the SHA-256 of its `.wasm` (hive-verify's
/// `LockedPlugin`, without the requirement it was resolved from).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedPlugin {
    pub module: String,
    pub version: String,
    /// Lowercase hex SHA-256 of the version's `.wasm`.
    pub sha256: String,
}

impl LockedPlugin {
    /// The item a fee for a call of it is charged on.
    pub fn item(&self) -> ItemRef {
        ItemRef {
            kind: ItemKind::Plugin,
            name: self.module.clone(),
            version: self.version.clone(),
        }
    }
}

/// One tool of a paid plugin, as the lockfile's manifest lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaidTool {
    /// The tool's own name, what `module.call` names.
    pub name: String,
    pub description: String,
    /// JSON Schema of the tool's arguments, as published.
    pub parameters_schema: String,
}

/// A paid plugin, as a published version's lockfile describes it: what a
/// worker builds the plugin's tools from. It holds no bytes and names none
/// to fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaidPluginManifest {
    pub plugin: LockedPlugin,
    pub description: String,
    pub tools: Vec<PaidTool>,
}

/// `module.call`'s params: one tool call of a paid plugin, which the
/// broker host runs in hive's module runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModuleCallParams {
    /// The exact version the caller's lockfile pins.
    pub plugin: LockedPlugin,
    pub tool: String,
    /// The model's arguments, a JSON object encoded once.
    pub arguments: String,
    /// The run the call is made from, as for `agent.invoke`: a worker's
    /// connection fills it in for the task it serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// `module.call`'s result: what the tool answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModuleCallOutcome {
    /// The tool's result: what the model sees.
    Result { content: String },
    /// The tool ran and refused or failed: what the model reads. Its fee
    /// is trued down to zero.
    ToolError { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_money_refusal_is_found_inside_a_wrapped_error_up_to_its_line_end() {
        let text = "Error: echo__reverse: fee_refused: plugin echo@1.0.0: cap, resets at \
                    2026-11-01T00:00:00Z\n\nStop now.";
        assert_eq!(
            find_money_refusal(text),
            Some("fee_refused: plugin echo@1.0.0: cap, resets at 2026-11-01T00:00:00Z")
        );
        assert_eq!(
            find_money_refusal("needs_acceptance: team auditor@2.0.0: accept its bill"),
            Some("needs_acceptance: team auditor@2.0.0: accept its bill")
        );
        assert_eq!(find_money_refusal("the analyst crashed"), None);
    }
}
