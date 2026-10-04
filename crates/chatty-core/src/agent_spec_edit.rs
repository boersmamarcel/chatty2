//! Writing agent specs from a form: Settings → Agents' New spec, Edit and
//! Duplicate, and a plugin's "Add to agent…" (PL-U5b, AGE-718).
//!
//! The form is text: one string per field, lists comma-separated. It edits
//! a base spec rather than replacing it, so whatever the form does not show
//! (a plugin's `config` and `limits`, the marketplace fields, any section
//! added later) is written back as it was read. Every save goes through
//! [`save_spec`]: [`AgentSpec::validate`] against the configured models,
//! then [`AgentSpec::to_toml`] into `<dir>/<name>.toml`. Nothing is written
//! while any error stands, and every error names the field it belongs to.
//!
//! A preset is never written: [`edit_target`] sends an edit of one to the
//! workspace (the data directory when there is none) under the same name,
//! where the new file shadows it.

use std::path::{Path, PathBuf};

use crate::agent_spec::{
    AgentSpec, Grant, PluginSpec, SpecError, SpecErrors, SpecSource, WORKSPACE_AGENTS_DIR,
};
use crate::settings::models::models_store::ModelConfig;

/// A field of the spec form, for placing an error next to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpecField {
    Name,
    Model,
    Profile,
    Disable,
    Plugins,
    MaxAgentTurns,
    MaxDuration,
    CapUsd,
    /// Not one field: the file itself (a write failed, a name is taken).
    File,
}

/// One problem with a form, and the field it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldError {
    pub field: SpecField,
    pub message: String,
}

impl FieldError {
    fn new(field: SpecField, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl From<&SpecError> for FieldError {
    fn from(error: &SpecError) -> Self {
        let field = match error {
            SpecError::BadName(_) | SpecError::ReservedName(_) => SpecField::Name,
            SpecError::BadModel(_) => SpecField::Model,
            SpecError::UnknownProfile(_) => SpecField::Profile,
            SpecError::UnknownToolGroup(_) => SpecField::Disable,
            SpecError::DuplicatePlugin(_)
            | SpecError::BadPlugin(_)
            | SpecError::UnrequestedGrant(_) => SpecField::Plugins,
            SpecError::BadDuration(_) => SpecField::MaxDuration,
            SpecError::BadCap(_) => SpecField::CapUsd,
        };
        Self::new(field, error.to_string())
    }
}

/// One `[[plugins]]` entry as the form shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginForm {
    pub module: String,
    /// A semver requirement; empty for none.
    pub version: String,
    /// Grant names, comma-separated: `llm, config`.
    pub grants: String,
}

/// A spec as the form edits it: text per field, lists comma-separated,
/// empty for unset.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpecForm {
    pub name: String,
    pub description: String,
    /// A model reference, resolved like `--model`; empty runs the default.
    pub model: String,
    pub preamble: String,
    /// A tool profile name; empty for the full tool set.
    pub profile: String,
    pub disable: String,
    pub skills: String,
    pub plugins: Vec<PluginForm>,
    pub delegates_to: String,
    pub exposed: bool,
    /// Empty: anyone may call it.
    pub callers: String,
    pub max_agent_turns: String,
    pub max_duration: String,
    pub cap_usd: String,
}

fn join(items: &[String]) -> String {
    items.join(", ")
}

fn split(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn optional(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

impl SpecForm {
    /// The form for a blank spec: exposed, everything else unset.
    pub fn blank() -> Self {
        Self::from_spec(&AgentSpec::default())
    }

    /// The form showing `spec`.
    pub fn from_spec(spec: &AgentSpec) -> Self {
        Self {
            name: spec.agent.name.clone(),
            description: spec.agent.description.clone().unwrap_or_default(),
            model: spec.agent.model.clone().unwrap_or_default(),
            preamble: spec.agent.preamble.clone().unwrap_or_default(),
            profile: spec.tools.profile.clone().unwrap_or_default(),
            disable: join(&spec.tools.disable),
            skills: join(&spec.tools.skills),
            plugins: spec
                .plugins
                .iter()
                .map(|plugin| PluginForm {
                    module: plugin.module.clone(),
                    version: plugin.version.clone().unwrap_or_default(),
                    grants: plugin
                        .grants
                        .iter()
                        .map(Grant::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                })
                .collect(),
            delegates_to: join(&spec.swarm.delegates_to),
            exposed: spec.swarm.exposed,
            callers: spec.swarm.callers.as_deref().map(join).unwrap_or_default(),
            max_agent_turns: spec
                .budget
                .max_agent_turns
                .map(|turns| turns.to_string())
                .unwrap_or_default(),
            max_duration: spec.budget.max_duration.clone().unwrap_or_default(),
            cap_usd: spec
                .budget
                .cap_usd
                .map(|cap| cap.to_string())
                .unwrap_or_default(),
        }
    }

    /// `base` with this form's fields written over it. A field that does
    /// not parse (a turn count that is not a number, an unknown grant) is
    /// reported and left as `base` had it, so [`AgentSpec::validate`] can
    /// still report everything else at once.
    pub fn apply(&self, base: &AgentSpec) -> (AgentSpec, Vec<FieldError>) {
        let mut errors = Vec::new();
        let mut spec = base.clone();

        spec.agent.name = self.name.trim().to_string();
        spec.agent.description = optional(&self.description);
        spec.agent.model = optional(&self.model);
        spec.agent.preamble = optional(&self.preamble);

        spec.tools.profile = optional(&self.profile);
        spec.tools.disable = split(&self.disable);
        spec.tools.skills = split(&self.skills);

        spec.plugins = self
            .plugins
            .iter()
            .map(|form| {
                let module = form.module.trim().to_string();
                // Keep what the form does not show: config, limits.
                let mut plugin = base
                    .plugins
                    .iter()
                    .find(|plugin| plugin.module == module)
                    .cloned()
                    .unwrap_or_else(|| PluginSpec {
                        module: module.clone(),
                        ..PluginSpec::default()
                    });
                plugin.version = optional(&form.version);
                plugin.grants = Vec::new();
                for grant in split(&form.grants) {
                    match grant.parse::<Grant>() {
                        Ok(grant) => plugin.grants.push(grant),
                        Err(why) => {
                            errors.push(FieldError::new(
                                SpecField::Plugins,
                                format!("plugins: `{module}`: {why}"),
                            ));
                        }
                    }
                }
                plugin
            })
            .collect();

        spec.swarm.delegates_to = split(&self.delegates_to);
        spec.swarm.exposed = self.exposed;
        let callers = split(&self.callers);
        // `callers = []` (nobody) and unset (anybody) both show as empty:
        // an untouched empty field keeps whichever the spec had.
        spec.swarm.callers = if callers.is_empty() {
            base.swarm.callers.clone().filter(|c| c.is_empty())
        } else {
            Some(callers)
        };

        spec.budget.max_agent_turns = match optional(&self.max_agent_turns) {
            None => None,
            Some(text) => match text.parse::<u32>() {
                Ok(turns) => Some(turns),
                Err(_) => {
                    errors.push(FieldError::new(
                        SpecField::MaxAgentTurns,
                        format!(
                            "budget.max_agent_turns '{text}' is not a whole number (0 = uncapped)"
                        ),
                    ));
                    base.budget.max_agent_turns
                }
            },
        };
        spec.budget.max_duration = optional(&self.max_duration);
        spec.budget.cap_usd = match optional(&self.cap_usd) {
            None => None,
            Some(text) => match text.trim_start_matches('$').parse::<f64>() {
                Ok(cap) => Some(cap),
                Err(_) => {
                    errors.push(FieldError::new(
                        SpecField::CapUsd,
                        format!("budget.cap_usd '{text}' is not an amount in dollars"),
                    ));
                    base.budget.cap_usd
                }
            },
        };

        (spec, errors)
    }
}

/// Where a form writes: a spec directory, and the file it replaces when
/// the edit renames it.
#[derive(Clone, Debug, PartialEq)]
pub struct SaveTarget {
    pub dir: PathBuf,
    /// The file being edited; `None` for a new spec. A rename writes the
    /// new file, then removes this one.
    pub previous: Option<PathBuf>,
}

/// The two spec directories a new spec can go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecHome {
    /// `<workspace>/.chatty/agents/`
    Workspace,
    /// `<data_dir>/chatty/agents/`
    DataDir,
}

/// The directory `home` names, `None` when there is no workspace (or data
/// directory) to put it in.
pub fn spec_dir(home: SpecHome, workspace: Option<&Path>, data_dir: Option<&Path>) -> Option<PathBuf> {
    match home {
        SpecHome::Workspace => workspace.map(|root| root.join(WORKSPACE_AGENTS_DIR)),
        SpecHome::DataDir => data_dir.map(|root| root.join("chatty").join("agents")),
    }
}

/// Where an edit of the spec found at `source` is saved. A file is
/// rewritten in place; a preset is never touched, so its edit goes to the
/// workspace under the same name and shadows it (the data directory when
/// no workspace is set).
pub fn edit_target(
    source: &SpecSource,
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
) -> Option<SaveTarget> {
    match source {
        SpecSource::Workspace(path) | SpecSource::DataDir(path) => Some(SaveTarget {
            dir: path.parent()?.to_path_buf(),
            previous: Some(path.clone()),
        }),
        SpecSource::Preset => spec_dir(SpecHome::Workspace, workspace, data_dir)
            .or_else(|| spec_dir(SpecHome::DataDir, workspace, data_dir))
            .map(|dir| SaveTarget {
                dir,
                previous: None,
            }),
    }
}

/// Where a duplicate of the spec found at `source` goes: beside the file,
/// or the workspace for a preset (as [`edit_target`], but always new).
pub fn duplicate_target(
    source: &SpecSource,
    workspace: Option<&Path>,
    data_dir: Option<&Path>,
) -> Option<SaveTarget> {
    edit_target(source, workspace, data_dir).map(|target| SaveTarget {
        previous: None,
        ..target
    })
}

/// A name for a copy of `name` that `taken` does not hold:
/// `<name>-copy`, then `<name>-copy-2`, `-3`, … Spec names are at most 64
/// characters, so a long name is cut to fit.
pub fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let candidate = |suffix: &str| {
        let keep = 64usize.saturating_sub(suffix.len()).min(name.len());
        let stem = name[..keep].trim_end_matches(['-', '_']);
        format!("{stem}{suffix}")
    };
    let first = candidate("-copy");
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| candidate(&format!("-copy-{n}")))
        .find(|name| !taken(name))
        .expect("an unbounded range finds a free name")
}

/// Check `spec` against the configured `models` and write it to
/// `<target.dir>/<name>.toml`. On any error nothing is written and every
/// error is returned. A new spec may not take the name of a file already
/// in the directory.
pub fn save_spec(
    spec: &AgentSpec,
    target: &SaveTarget,
    models: Option<&[ModelConfig]>,
) -> Result<PathBuf, Vec<FieldError>> {
    save_checked(spec, target, models, Vec::new())
}

/// [`SpecForm::apply`] onto `base`, then [`save_spec`], reporting the
/// form's parse errors and the spec's validation errors together.
pub fn save_form(
    form: &SpecForm,
    base: &AgentSpec,
    target: &SaveTarget,
    models: Option<&[ModelConfig]>,
) -> Result<PathBuf, Vec<FieldError>> {
    let (spec, errors) = form.apply(base);
    save_checked(&spec, target, models, errors)
}

fn save_checked(
    spec: &AgentSpec,
    target: &SaveTarget,
    models: Option<&[ModelConfig]>,
    mut errors: Vec<FieldError>,
) -> Result<PathBuf, Vec<FieldError>> {
    if let Err(SpecErrors(invalid)) = spec.validate(models) {
        errors.extend(invalid.iter().map(FieldError::from));
    }
    let path = target.dir.join(format!("{}.toml", spec.agent.name));
    if errors.is_empty() && path.exists() && target.previous.as_deref() != Some(path.as_path()) {
        errors.push(FieldError::new(
            SpecField::Name,
            format!(
                "agent.name '{}' is taken: {} exists",
                spec.agent.name,
                path.display()
            ),
        ));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    write_spec(spec, &path).map_err(|e| vec![FieldError::new(SpecField::File, format!("{e:#}"))])?;
    if let Some(previous) = target.previous.as_deref()
        && previous != path
        && let Err(e) = std::fs::remove_file(previous)
    {
        return Err(vec![FieldError::new(
            SpecField::File,
            format!(
                "saved {}, but could not remove the old {}: {e}",
                path.display(),
                previous.display()
            ),
        )]);
    }
    Ok(path)
}

/// Write through a sibling temporary file, so a reader never sees half a
/// spec.
fn write_spec(spec: &AgentSpec, path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = path.parent().context("a spec path has a directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let text = spec.to_toml()?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// "Add to agent…": `spec` with `module` appended under `[[plugins]]`,
/// granted `grants`. A spec that already lists the module is an error.
pub fn with_plugin(
    spec: &AgentSpec,
    module: &str,
    version: Option<String>,
    grants: Vec<Grant>,
) -> Result<AgentSpec, FieldError> {
    if spec.plugins.iter().any(|plugin| plugin.module == module) {
        return Err(FieldError::new(
            SpecField::Plugins,
            format!(
                "{} already lists the plugin '{module}': edit its grants there",
                spec.agent.name
            ),
        ));
    }
    let mut spec = spec.clone();
    spec.plugins.push(PluginSpec {
        module: module.to_string(),
        version,
        grants,
        ..PluginSpec::default()
    });
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::{PRESETS, load_agent_spec_from};
    use crate::settings::models::providers_store::ProviderType;

    fn models() -> Vec<ModelConfig> {
        vec![ModelConfig::new(
            "qwen3:4b".to_string(),
            "qwen3:4b".to_string(),
            ProviderType::Ollama,
            "qwen3:4b".to_string(),
        )]
    }

    fn full_form() -> SpecForm {
        SpecForm {
            name: "auditor".to_string(),
            description: "Audits number sets".to_string(),
            model: "qwen3:4b".to_string(),
            preamble: "You audit financial datasets.\nCheck twice.".to_string(),
            profile: "reviewer".to_string(),
            disable: "shell".to_string(),
            skills: "benford-audit, coder-reviewer".to_string(),
            plugins: vec![PluginForm {
                module: "benford".to_string(),
                version: "^0.2".to_string(),
                grants: "llm, config".to_string(),
            }],
            delegates_to: "data-analyst, *-reviewer".to_string(),
            exposed: false,
            callers: "data-lead".to_string(),
            max_agent_turns: "12".to_string(),
            max_duration: "30m".to_string(),
            cap_usd: "2.5".to_string(),
        }
    }

    fn workspace_target(ws: &Path) -> SaveTarget {
        SaveTarget {
            dir: spec_dir(SpecHome::Workspace, Some(ws), None).unwrap(),
            previous: None,
        }
    }

    #[test]
    fn spec_form_round_trips() {
        let ws = tempfile::tempdir().unwrap();
        let form = full_form();
        let path = save_form(
            &form,
            &AgentSpec::default(),
            &workspace_target(ws.path()),
            Some(&models()),
        )
        .expect("the full form saves");
        assert_eq!(path, ws.path().join(".chatty/agents/auditor.toml"));

        let loaded = load_agent_spec_from("auditor", Some(ws.path()), None).unwrap();
        assert!(matches!(loaded.source, SpecSource::Workspace(_)));
        let (expected, errors) = form.apply(&AgentSpec::default());
        assert!(errors.is_empty());
        assert_eq!(loaded.spec, expected);
        assert_eq!(SpecForm::from_spec(&loaded.spec), form);

        // What the form does not show survives an edit through it.
        let mut base = loaded.spec.clone();
        base.agent.example_prompt = Some("Audit ledger.csv".to_string());
        base.plugins[0].config.insert("threshold".to_string(), "0.05".to_string());
        base.plugins[0].limits.max_memory_mb = Some(64);
        let (edited, _) = SpecForm::from_spec(&base).apply(&base);
        assert_eq!(edited, base);
    }

    #[test]
    fn saving_an_invalid_spec_shows_every_error() {
        let ws = tempfile::tempdir().unwrap();
        let form = SpecForm {
            model: "no-such-model".to_string(),
            profile: "wizard".to_string(),
            cap_usd: "-1".to_string(),
            max_agent_turns: "many".to_string(),
            ..full_form()
        };
        let errors = save_form(
            &form,
            &AgentSpec::default(),
            &workspace_target(ws.path()),
            Some(&models()),
        )
        .unwrap_err();
        let fields: Vec<SpecField> = errors.iter().map(|e| e.field).collect();
        for field in [
            SpecField::Model,
            SpecField::Profile,
            SpecField::CapUsd,
            SpecField::MaxAgentTurns,
        ] {
            assert!(fields.contains(&field), "{field:?} missing from {errors:?}");
        }
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("no-such-model"))
        );
        assert!(!ws.path().join(".chatty").exists(), "nothing is written");

        // A cap that is not a number at all is reported too.
        let form = SpecForm {
            cap_usd: "lots".to_string(),
            ..full_form()
        };
        let errors = save_form(
            &form,
            &AgentSpec::default(),
            &workspace_target(ws.path()),
            Some(&models()),
        )
        .unwrap_err();
        assert_eq!(errors[0].field, SpecField::CapUsd);
        assert!(!ws.path().join(".chatty").exists());
    }

    #[test]
    fn edit_on_a_preset_duplicates_into_the_workspace() {
        let ws = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let (name, text) = PRESETS[0];
        let preset = load_agent_spec_from(name, Some(ws.path()), Some(data.path())).unwrap();
        assert_eq!(preset.source, SpecSource::Preset);

        let target = edit_target(&preset.source, Some(ws.path()), Some(data.path())).unwrap();
        assert_eq!(target.dir, ws.path().join(WORKSPACE_AGENTS_DIR));
        assert_eq!(target.previous, None);

        let mut form = SpecForm::from_spec(&preset.spec);
        form.description = "My own take".to_string();
        save_form(&form, &preset.spec, &target, None).expect("the edit saves");

        let loaded = load_agent_spec_from(name, Some(ws.path()), Some(data.path())).unwrap();
        assert!(matches!(loaded.source, SpecSource::Workspace(_)));
        assert_eq!(loaded.spec.agent.description.as_deref(), Some("My own take"));
        // The preset itself is untouched and still compiled in.
        assert_eq!(PRESETS[0].1, text);
        assert!(!data.path().join("chatty").exists());

        // Duplicate picks a free name next to it.
        let taken = |n: &str| n == format!("{name}-copy");
        assert_eq!(free_name(name, taken), format!("{name}-copy-2"));
        let dup = duplicate_target(&loaded.source, Some(ws.path()), None).unwrap();
        assert_eq!(dup.previous, None);
        assert_eq!(dup.dir, ws.path().join(WORKSPACE_AGENTS_DIR));
        // Saving a new spec under a taken name is refused.
        let err = save_spec(&loaded.spec, &dup, None).unwrap_err();
        assert_eq!(err[0].field, SpecField::Name);
        assert_eq!(free_name(&"a".repeat(64), |_| false).len(), 64);
    }

    #[test]
    fn add_to_agent_appends_a_plugin_with_its_grants() {
        let ws = tempfile::tempdir().unwrap();
        let target = workspace_target(ws.path());
        let mut spec = AgentSpec::named("auditor");
        spec.plugins.push(PluginSpec {
            module: "csv".to_string(),
            ..PluginSpec::default()
        });
        save_spec(&spec, &target, None).unwrap();

        let loaded = load_agent_spec_from("auditor", Some(ws.path()), None).unwrap();
        let target = edit_target(&loaded.source, Some(ws.path()), None).unwrap();
        let added = with_plugin(
            &loaded.spec,
            "benford",
            Some("^0.2".to_string()),
            vec![Grant::Llm, Grant::Config],
        )
        .unwrap();
        save_spec(&added, &target, None).unwrap();

        let reread = load_agent_spec_from("auditor", Some(ws.path()), None).unwrap();
        assert_eq!(reread.spec.plugins.len(), 2);
        assert_eq!(reread.spec.plugins[0].module, "csv");
        let plugin = &reread.spec.plugins[1];
        assert_eq!(plugin.module, "benford");
        assert_eq!(plugin.version.as_deref(), Some("^0.2"));
        assert_eq!(plugin.grants, vec![Grant::Llm, Grant::Config]);

        // Adding it twice is refused rather than duplicated.
        let again = with_plugin(&reread.spec, "benford", None, vec![]).unwrap_err();
        assert_eq!(again.field, SpecField::Plugins);
    }

    #[test]
    fn renaming_a_spec_moves_its_file() {
        let ws = tempfile::tempdir().unwrap();
        let path = save_spec(&AgentSpec::named("old"), &workspace_target(ws.path()), None).unwrap();
        let loaded = load_agent_spec_from("old", Some(ws.path()), None).unwrap();
        let target = edit_target(&loaded.source, Some(ws.path()), None).unwrap();
        assert_eq!(target.previous.as_deref(), Some(path.as_path()));
        let form = SpecForm {
            name: "new".to_string(),
            ..SpecForm::from_spec(&loaded.spec)
        };
        save_form(&form, &loaded.spec, &target, None).unwrap();
        assert!(!path.exists());
        assert!(load_agent_spec_from("new", Some(ws.path()), None).is_ok());
    }
}
