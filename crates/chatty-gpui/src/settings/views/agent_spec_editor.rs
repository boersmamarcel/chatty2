//! The spec form behind Settings → Agents' New spec, Edit and Duplicate,
//! and a plugin's "Add to agent…" (PL-U5b, AGE-718).
//!
//! The dialogs hold text; [`chatty_core::agent_spec_edit`] turns it into a
//! spec over the one it was opened on, validates it against the configured
//! models and writes it. Every error is shown under its field and nothing
//! is written until there are none. After a save the specs are re-read and
//! the module runtime refreshed, so the broker serves the new spec without
//! a restart.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use chatty_core::agent_spec::{AgentSpec, Grant, SpecListing, SpecSource};
use chatty_core::agent_spec_edit::{
    FieldError, PluginForm, SaveTarget, SpecField, SpecForm, SpecHome, duplicate_target,
    edit_target, free_name, save_form, save_spec, spec_dir, with_plugin,
};
use chatty_core::factories::agent_factory::tool_profile_names;
use chatty_core::settings::models::models_store::resolve_model_query;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{Select, SelectState};
use gpui_component::{
    ActiveTheme, Disableable, IndexPath, Sizable, WindowExt as _, h_flex, v_flex,
};

use crate::settings::controllers::module_settings_controller;
use crate::settings::models::agent_specs::workspace;
use crate::settings::models::{AgentSpecsModel, DiscoveredModulesModel, ModelsModel};

/// Every grant a plugin can be given, for a plugin whose `metadata` has
/// not been read (it has not loaded).
const ALL_GRANTS: &[&str] = &["llm", "config", "file", "billing"];

fn workspace_dir(cx: &App) -> Option<PathBuf> {
    workspace(cx).map(PathBuf::from)
}

fn data_dir() -> Option<PathBuf> {
    dirs::data_dir()
}

fn taken_names(cx: &mut App) -> Vec<String> {
    AgentSpecsModel::listings(cx)
        .into_iter()
        .map(|listing| listing.name)
        .collect()
}

/// After a save: re-read the specs and refresh the runtime, so the roster
/// serves the new spec now.
fn after_save(cx: &mut App) {
    AgentSpecsModel::reload(cx);
    module_settings_controller::refresh_runtime(cx);
    cx.refresh_windows();
}

/// What the form is editing, and where a save goes.
enum Destination {
    /// A new spec; the user picks the directory.
    New(Rc<Cell<SpecHome>>),
    /// A fixed place: an edit in place, an edit of a preset, a duplicate.
    Fixed(SaveTarget),
}

/// Settings → Agents → New spec.
pub fn open_new_spec(window: &mut Window, cx: &mut App) {
    let home = if workspace_dir(cx).is_some() {
        SpecHome::Workspace
    } else {
        SpecHome::DataDir
    };
    open_editor(
        "New agent spec".to_string(),
        AgentSpec::default(),
        SpecForm::blank(),
        Destination::New(Rc::new(Cell::new(home))),
        window,
        cx,
    );
}

/// A row's Edit: the file in place, or, for a preset, a copy under the same
/// name in the workspace that shadows it.
pub fn open_edit_spec(listing: &SpecListing, window: &mut Window, cx: &mut App) {
    let Ok(spec) = listing.spec.clone() else {
        return;
    };
    let Some(target) = edit_target(&listing.source, workspace_dir(cx).as_deref(), data_dir().as_deref())
    else {
        return;
    };
    let title = match listing.source {
        SpecSource::Preset => format!("Edit {} (saved to {}, shadowing the preset)", listing.name, target.dir.display()),
        _ => format!("Edit {}", listing.name),
    };
    let form = SpecForm::from_spec(&spec);
    open_editor(title, spec, form, Destination::Fixed(target), window, cx);
}

/// A row's Duplicate: the same spec under a free name, beside it (in the
/// workspace for a preset).
pub fn open_duplicate_spec(listing: &SpecListing, window: &mut Window, cx: &mut App) {
    let Ok(spec) = listing.spec.clone() else {
        return;
    };
    let Some(target) =
        duplicate_target(&listing.source, workspace_dir(cx).as_deref(), data_dir().as_deref())
    else {
        return;
    };
    let taken = taken_names(cx);
    let name = free_name(&listing.name, |candidate| {
        taken.iter().any(|t| t == candidate) || target.dir.join(format!("{candidate}.toml")).exists()
    });
    let mut form = SpecForm::from_spec(&spec);
    form.name = name;
    open_editor(
        format!("Duplicate {}", listing.name),
        spec,
        form,
        Destination::Fixed(target),
        window,
        cx,
    );
}

fn text_input(
    value: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let value = value.to_string();
    let placeholder = placeholder.to_string();
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder(placeholder);
        state.set_value(value, window, cx);
        state
    })
}

/// A select over `(label, value)` options, with `current` selected; a
/// `current` that is none of them is added, so validation can name it.
struct Choice {
    state: Entity<SelectState<Vec<String>>>,
    values: Vec<String>,
}

impl Choice {
    fn new(
        mut options: Vec<(String, String)>,
        current: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        if !options.iter().any(|(_, value)| value == current) {
            options.push((format!("{current} (not configured)"), current.to_string()));
        }
        let index = options.iter().position(|(_, value)| value == current);
        let labels: Vec<String> = options.iter().map(|(label, _)| label.clone()).collect();
        let values = options.into_iter().map(|(_, value)| value).collect();
        let state = cx.new(|cx| SelectState::new(labels, index.map(IndexPath::new), window, cx));
        Self { state, values }
    }

    fn value(&self, cx: &App) -> String {
        self.state
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.values.get(index.row).cloned())
            .unwrap_or_default()
    }
}

struct PluginRow {
    module: String,
    version: Entity<InputState>,
    grants: Entity<InputState>,
}

struct Editor {
    name: Entity<InputState>,
    description: Entity<InputState>,
    model: Choice,
    preamble: Entity<InputState>,
    profile: Choice,
    disable: Entity<InputState>,
    skills: Entity<InputState>,
    plugins: Rc<RefCell<Vec<PluginRow>>>,
    add_plugin: Choice,
    delegates_to: Entity<InputState>,
    exposed: Rc<Cell<bool>>,
    callers: Entity<InputState>,
    max_agent_turns: Entity<InputState>,
    max_duration: Entity<InputState>,
    cap_usd: Entity<InputState>,
}

impl Editor {
    fn form(&self, cx: &App) -> SpecForm {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        SpecForm {
            name: text(&self.name),
            description: text(&self.description),
            model: self.model.value(cx),
            preamble: text(&self.preamble),
            profile: self.profile.value(cx),
            disable: text(&self.disable),
            skills: text(&self.skills),
            plugins: self
                .plugins
                .borrow()
                .iter()
                .map(|row| PluginForm {
                    module: row.module.clone(),
                    version: text(&row.version),
                    grants: text(&row.grants),
                })
                .collect(),
            delegates_to: text(&self.delegates_to),
            exposed: self.exposed.get(),
            callers: text(&self.callers),
            max_agent_turns: text(&self.max_agent_turns),
            max_duration: text(&self.max_duration),
            cap_usd: text(&self.cap_usd),
        }
    }
}

fn plugin_row(plugin: &PluginForm, window: &mut Window, cx: &mut App) -> PluginRow {
    PluginRow {
        module: plugin.module.clone(),
        version: text_input(&plugin.version, "any version", window, cx),
        grants: text_input(&plugin.grants, "llm, config", window, cx),
    }
}

fn installed_modules(cx: &App) -> Vec<String> {
    cx.try_global::<DiscoveredModulesModel>()
        .map(|dm| dm.modules.iter().map(|m| m.name.clone()).collect())
        .unwrap_or_default()
}

fn open_editor(
    title: String,
    base: AgentSpec,
    form: SpecForm,
    destination: Destination,
    window: &mut Window,
    cx: &mut App,
) {
    let models = cx.global::<ModelsModel>().models().to_vec();
    // The model the spec names, as the select knows it: its configured
    // name when it resolves, else the reference as written.
    let current_model = if form.model.is_empty() {
        String::new()
    } else {
        resolve_model_query(&models, Some(&form.model))
            .map(|model| model.name.clone())
            .unwrap_or_else(|| form.model.clone())
    };
    let mut model_options = vec![("The roster's default model".to_string(), String::new())];
    model_options.extend(models.iter().map(|model| {
        (
            format!("{} ({})", model.name, model.model_identifier),
            model.name.clone(),
        )
    }));
    let mut profile_options = vec![("The full tool set".to_string(), String::new())];
    profile_options.extend(
        tool_profile_names()
            .into_iter()
            .map(|name| (name.to_string(), name.to_string())),
    );
    let mut module_options = vec![("Add a plugin…".to_string(), String::new())];
    module_options.extend(
        installed_modules(cx)
            .into_iter()
            .map(|name| (name.clone(), name)),
    );

    let editor = Rc::new(Editor {
        name: text_input(&form.name, "lowercase-name", window, cx),
        description: text_input(&form.description, "What it is for", window, cx),
        model: Choice::new(model_options, &current_model, window, cx),
        preamble: {
            let value = form.preamble.clone();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .multi_line(true)
                    .rows(4)
                    .placeholder("Its standing instructions");
                state.set_value(value, window, cx);
                state
            })
        },
        profile: Choice::new(profile_options, &form.profile, window, cx),
        disable: text_input(&form.disable, "tool groups, e.g. shell, fs-write", window, cx),
        skills: text_input(&form.skills, "skill names", window, cx),
        plugins: Rc::new(RefCell::new(
            form.plugins
                .iter()
                .map(|plugin| plugin_row(plugin, window, cx))
                .collect(),
        )),
        add_plugin: Choice::new(module_options, "", window, cx),
        delegates_to: text_input(&form.delegates_to, "agent names or globs", window, cx),
        exposed: Rc::new(Cell::new(form.exposed)),
        callers: text_input(&form.callers, "anyone", window, cx),
        max_agent_turns: text_input(&form.max_agent_turns, "0 = uncapped", window, cx),
        max_duration: text_input(&form.max_duration, "e.g. 30m", window, cx),
        cap_usd: text_input(&form.cap_usd, "dollars per task", window, cx),
    });
    let errors: Rc<RefCell<Vec<FieldError>>> = Rc::new(RefCell::new(Vec::new()));
    let destination = Rc::new(destination);
    let workspace = workspace_dir(cx);
    let data = data_dir();
    let base = Rc::new(base);

    window.open_dialog(cx, move |dialog, _window, cx| {
        let errs = errors.borrow().clone();
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let field_errors = |field: SpecField| -> Vec<AnyElement> {
            errs.iter()
                .filter(|e| e.field == field)
                .map(|e| {
                    div()
                        .text_xs()
                        .text_color(danger)
                        .child(e.message.clone())
                        .into_any_element()
                })
                .collect()
        };
        let labelled = |label: &str, field: Option<SpecField>, control: AnyElement| {
            v_flex()
                .gap_1()
                .child(div().text_xs().text_color(muted).child(label.to_string()))
                .child(control)
                .children(field.map(&field_errors).unwrap_or_default())
        };
        let heading = |text: &str| {
            div()
                .pt_2()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(text.to_string())
        };

        let location = match destination.as_ref() {
            Destination::Fixed(target) => div()
                .text_xs()
                .text_color(muted)
                .child(format!("Saved to {}", target.dir.display()))
                .into_any_element(),
            Destination::New(home) => {
                let chip = |id: &'static str, label: &str, value: SpecHome, enabled: bool| {
                    let selected = home.get() == value;
                    let home = home.clone();
                    let button = Button::new(id)
                        .small()
                        .label(label.to_string())
                        .disabled(!enabled)
                        .on_click(move |_, _, cx| {
                            home.set(value);
                            cx.refresh_windows();
                        });
                    if selected {
                        button.primary()
                    } else {
                        button.outline()
                    }
                };
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("Save in"))
                    .child(chip(
                        "spec-home-workspace",
                        "Workspace (.chatty/agents)",
                        SpecHome::Workspace,
                        workspace.is_some(),
                    ))
                    .child(chip(
                        "spec-home-data",
                        "Data dir (chatty/agents)",
                        SpecHome::DataDir,
                        data.is_some(),
                    ))
                    .into_any_element()
            }
        };

        let plugin_rows: Vec<AnyElement> = editor
            .plugins
            .borrow()
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let plugins = editor.plugins.clone();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(120.)).text_sm().child(row.module.clone()))
                    .child(div().w(px(120.)).child(Input::new(&row.version).small()))
                    .child(div().flex_1().child(Input::new(&row.grants).small()))
                    .child(
                        Button::new(SharedString::from(format!("spec-plugin-remove-{index}")))
                            .small()
                            .ghost()
                            .label("Remove")
                            .on_click(move |_, _, cx| {
                                plugins.borrow_mut().remove(index);
                                cx.refresh_windows();
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        let general: Vec<AnyElement> = field_errors(SpecField::File);
        let error_count = errs.len();

        dialog
            .title(title.clone())
            .overlay(true)
            .keyboard(true)
            .close_button(true)
            .w(px(720.))
            .child(
                div()
                    .id("spec-editor-form")
                    .overflow_y_scrollbar()
                    .max_h(px(560.))
                    .child(
                        v_flex()
                            .gap_2()
                            .px_4()
                            .pb_2()
                            .child(location)
                            .child(heading("[agent]"))
                            .child(labelled(
                                "Name",
                                Some(SpecField::Name),
                                Input::new(&editor.name).small().into_any_element(),
                            ))
                            .child(labelled(
                                "Description",
                                None,
                                Input::new(&editor.description).small().into_any_element(),
                            ))
                            .child(labelled(
                                "Model",
                                Some(SpecField::Model),
                                Select::new(&editor.model.state).small().into_any_element(),
                            ))
                            .child(labelled(
                                "Preamble",
                                None,
                                Input::new(&editor.preamble).into_any_element(),
                            ))
                            .child(heading("[tools]"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(labelled(
                                        "Profile",
                                        Some(SpecField::Profile),
                                        Select::new(&editor.profile.state).small().into_any_element(),
                                    )))
                                    .child(div().flex_1().child(labelled(
                                        "Disable",
                                        Some(SpecField::Disable),
                                        Input::new(&editor.disable).small().into_any_element(),
                                    ))),
                            )
                            .child(labelled(
                                "Skills",
                                None,
                                Input::new(&editor.skills).small().into_any_element(),
                            ))
                            .child(heading("[[plugins]]"))
                            .when(plugin_rows.is_empty(), |el| {
                                el.child(div().text_xs().text_color(muted).child("No plugins."))
                            })
                            .when(!plugin_rows.is_empty(), |el| {
                                el.child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(div().w(px(120.)).child("Module"))
                                        .child(div().w(px(120.)).child("Version"))
                                        .child(div().flex_1().child("Grants")),
                                )
                            })
                            .children(plugin_rows)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .w(px(240.))
                                            .child(Select::new(&editor.add_plugin.state).small()),
                                    )
                                    .child({
                                        let editor = editor.clone();
                                        Button::new("spec-plugin-add")
                                            .small()
                                            .outline()
                                            .label("Add")
                                            .on_click(move |_, window, cx| {
                                                let module = editor.add_plugin.value(cx);
                                                if module.is_empty()
                                                    || editor
                                                        .plugins
                                                        .borrow()
                                                        .iter()
                                                        .any(|row| row.module == module)
                                                {
                                                    return;
                                                }
                                                let row = plugin_row(
                                                    &PluginForm {
                                                        module,
                                                        ..PluginForm::default()
                                                    },
                                                    window,
                                                    cx,
                                                );
                                                editor.plugins.borrow_mut().push(row);
                                                cx.refresh_windows();
                                            })
                                    }),
                            )
                            .children(field_errors(SpecField::Plugins))
                            .child(heading("[swarm]"))
                            .child(labelled(
                                "Delegates to",
                                None,
                                Input::new(&editor.delegates_to).small().into_any_element(),
                            ))
                            .child(
                                h_flex()
                                    .gap_4()
                                    .items_end()
                                    .child({
                                        let exposed = editor.exposed.clone();
                                        Checkbox::new("spec-exposed")
                                            .label("Exposed: others may call it")
                                            .checked(exposed.get())
                                            .on_click(move |checked, _, cx| {
                                                exposed.set(*checked);
                                                cx.refresh_windows();
                                            })
                                    })
                                    .child(div().flex_1().child(labelled(
                                        "Callers",
                                        None,
                                        Input::new(&editor.callers).small().into_any_element(),
                                    ))),
                            )
                            .child(heading("[budget]"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(labelled(
                                        "Max agent turns",
                                        Some(SpecField::MaxAgentTurns),
                                        Input::new(&editor.max_agent_turns).small().into_any_element(),
                                    )))
                                    .child(div().flex_1().child(labelled(
                                        "Max duration",
                                        Some(SpecField::MaxDuration),
                                        Input::new(&editor.max_duration).small().into_any_element(),
                                    )))
                                    .child(div().flex_1().child(labelled(
                                        "Cap (USD)",
                                        Some(SpecField::CapUsd),
                                        Input::new(&editor.cap_usd).small().into_any_element(),
                                    ))),
                            )
                            .children(general),
                    ),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(if error_count > 0 { danger } else { muted })
                            .child(if error_count > 0 {
                                format!(
                                    "{error_count} problem{} — nothing was saved",
                                    if error_count == 1 { "" } else { "s" }
                                )
                            } else {
                                "Checked against your configured models on save".to_string()
                            }),
                    )
                    .child(
                        Button::new("spec-editor-cancel")
                            .small()
                            .outline()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child({
                        let editor = editor.clone();
                        let errors = errors.clone();
                        let destination = destination.clone();
                        let base = base.clone();
                        let workspace = workspace.clone();
                        let data = data.clone();
                        Button::new("spec-editor-save")
                            .small()
                            .primary()
                            .label("Save")
                            .on_click(move |_, window, cx| {
                                let form = editor.form(cx);
                                let target = match destination.as_ref() {
                                    Destination::Fixed(target) => Some(target.clone()),
                                    Destination::New(home) => {
                                        spec_dir(home.get(), workspace.as_deref(), data.as_deref())
                                            .map(|dir| SaveTarget {
                                                dir,
                                                previous: None,
                                            })
                                    }
                                };
                                let Some(target) = target else {
                                    *errors.borrow_mut() = vec![FieldError {
                                        field: SpecField::File,
                                        message: "No directory to save in: set a workspace, or pick the data dir".to_string(),
                                    }];
                                    cx.refresh_windows();
                                    return;
                                };
                                let models = cx.global::<ModelsModel>().models().to_vec();
                                match save_form(&form, &base, &target, Some(&models)) {
                                    Ok(path) => {
                                        tracing::info!(path = %path.display(), "Saved agent spec");
                                        window.close_dialog(cx);
                                        after_save(cx);
                                    }
                                    Err(found) => {
                                        *errors.borrow_mut() = found;
                                        cx.refresh_windows();
                                    }
                                }
                            })
                    }),
            )
    });
}

/// "Add to agent…" for the plugin `module`: pick a spec (or name a new
/// one) and the grants, and append it under `[[plugins]]` through the same
/// save path as the form.
pub fn open_add_to_agent(module: &str, window: &mut Window, cx: &mut App) {
    let module = module.to_string();
    let requested: Vec<String> = cx
        .try_global::<DiscoveredModulesModel>()
        .and_then(|dm| dm.modules.iter().find(|m| m.name == module))
        .and_then(|m| m.requested.clone())
        .unwrap_or_else(|| ALL_GRANTS.iter().map(|g| g.to_string()).collect())
        .into_iter()
        .filter(|capability| capability != "logging")
        .collect();
    let listings: Vec<SpecListing> = AgentSpecsModel::listings(cx)
        .into_iter()
        .filter(|listing| !listing.shadowed && listing.spec.is_ok())
        .collect();
    const NEW: &str = "\u{0}new";
    let mut options: Vec<(String, String)> = listings
        .iter()
        .map(|listing| {
            let label = match listing.source {
                SpecSource::Preset => format!("{} (preset: saved as a workspace copy)", listing.name),
                _ => listing.name.clone(),
            };
            (label, listing.name.clone())
        })
        .collect();
    options.push(("New spec…".to_string(), NEW.to_string()));
    let first = options[0].1.clone();
    let target_choice = Rc::new(Choice::new(options, &first, window, cx));
    let new_name = text_input("", "name of the new spec", window, cx);
    let granted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let errors: Rc<RefCell<Vec<FieldError>>> = Rc::new(RefCell::new(Vec::new()));
    let listings = Rc::new(listings);
    let workspace = workspace_dir(cx);
    let data = data_dir();

    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let creating = target_choice.value(cx) == NEW;
        let errs = errors.borrow().clone();
        dialog
            .title(format!("Add {module} to an agent"))
            .overlay(true)
            .keyboard(true)
            .close_button(true)
            .w(px(520.))
            .child(
                v_flex()
                    .gap_3()
                    .px_4()
                    .pb_2()
                    .child(div().text_xs().text_color(muted).child(
                        "Appends a [[plugins]] entry to the spec; the plugin's tools become the \
                         agent's. Grant only what the agent needs.",
                    ))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child("Agent"))
                            .child(Select::new(&target_choice.state).small()),
                    )
                    .when(creating, |el| {
                        el.child(
                            v_flex()
                                .gap_1()
                                .child(div().text_xs().text_color(muted).child("New spec's name"))
                                .child(Input::new(&new_name).small()),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child(format!(
                                "Grants ({module} requests {}; logging is always on)",
                                if requested.is_empty() {
                                    "nothing else".to_string()
                                } else {
                                    requested.join(", ")
                                }
                            )))
                            .child(h_flex().gap_3().children(requested.iter().map(|capability| {
                                let checked = granted.borrow().contains(capability);
                                let granted = granted.clone();
                                let name = capability.clone();
                                Checkbox::new(SharedString::from(format!("add-grant-{capability}")))
                                    .label(capability.clone())
                                    .checked(checked)
                                    .on_click(move |checked, _, cx| {
                                        let mut granted = granted.borrow_mut();
                                        granted.retain(|g| g != &name);
                                        if *checked {
                                            granted.push(name.clone());
                                        }
                                        drop(granted);
                                        cx.refresh_windows();
                                    })
                            }))),
                    )
                    .children(errs.iter().map(|e| {
                        div()
                            .text_xs()
                            .text_color(danger)
                            .child(e.message.clone())
                    })),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_3()
                    .justify_end()
                    .child(
                        Button::new("add-to-agent-cancel")
                            .small()
                            .outline()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child({
                        let module = module.clone();
                        let target_choice = target_choice.clone();
                        let new_name = new_name.clone();
                        let granted = granted.clone();
                        let errors = errors.clone();
                        let listings = listings.clone();
                        let requested = requested.clone();
                        let workspace = workspace.clone();
                        let data = data.clone();
                        Button::new("add-to-agent-save")
                            .small()
                            .primary()
                            .label("Add")
                            .on_click(move |_, window, cx| {
                                let choice = target_choice.value(cx);
                                // Grants in the order the plugin requests them.
                                let grants: Vec<Grant> = requested
                                    .iter()
                                    .filter(|r| granted.borrow().contains(r))
                                    .filter_map(|g| g.parse().ok())
                                    .collect();
                                let picked = if choice == NEW {
                                    let name = new_name.read(cx).value().trim().to_string();
                                    let home = if workspace.is_some() {
                                        SpecHome::Workspace
                                    } else {
                                        SpecHome::DataDir
                                    };
                                    spec_dir(home, workspace.as_deref(), data.as_deref()).map(|dir| {
                                        (AgentSpec::named(&name), SaveTarget { dir, previous: None })
                                    })
                                } else {
                                    listings.iter().find(|l| l.name == choice).and_then(|listing| {
                                        let spec = listing.spec.clone().ok()?;
                                        let target = edit_target(
                                            &listing.source,
                                            workspace.as_deref(),
                                            data.as_deref(),
                                        )?;
                                        Some((spec, target))
                                    })
                                };
                                let Some((spec, target)) = picked else {
                                    *errors.borrow_mut() = vec![FieldError {
                                        field: SpecField::File,
                                        message: "No directory to save in: set a workspace".to_string(),
                                    }];
                                    cx.refresh_windows();
                                    return;
                                };
                                let models = cx.global::<ModelsModel>().models().to_vec();
                                let result = with_plugin(&spec, &module, None, grants)
                                    .map_err(|e| vec![e])
                                    .and_then(|spec| save_spec(&spec, &target, Some(&models)));
                                match result {
                                    Ok(path) => {
                                        tracing::info!(path = %path.display(), module = %module, "Added plugin to agent spec");
                                        window.close_dialog(cx);
                                        after_save(cx);
                                    }
                                    Err(found) => {
                                        *errors.borrow_mut() = found;
                                        cx.refresh_windows();
                                    }
                                }
                            })
                    }),
            )
    });
}
