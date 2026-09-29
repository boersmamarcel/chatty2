//! The `coder-reviewer` team as a test fixture (AGE-752). It no longer
//! ships as a preset, but it is still the team `spec_golden`'s recorded
//! contexts and the team-mechanics tests (the skill instruction, the
//! delegation policy, the preset-vs-workspace lookup) run against. Its
//! directory is a workspace root: `.chatty/agents/` and `.chatty/teams/`.

use std::path::PathBuf;

use chatty_core::services::team::{Team, load_team};

/// The fixture team's id.
pub(crate) const TEAM: &str = "coder-reviewer";

/// The workspace root the fixture team and its specs live under.
pub(crate) fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team-workspace")
}

/// The fixture team, loaded from [`workspace`].
pub(crate) fn load() -> Team {
    load_team(TEAM, Some(&workspace()), None).expect("the fixture team loads")
}
