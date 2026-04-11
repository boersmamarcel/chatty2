use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::engine::{ChatEngine, Command, EngineAction, NavigableList};
use crate::events::AppEvent;
use crate::ui::{self, InputState};

/// Run the interactive TUI application
pub async fn run(
    mut engine: ChatEngine,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut engine, &mut event_rx).await;
    ratatui::restore();
    result
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    engine: &mut ChatEngine,
    event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    let mut input_state = InputState::new();

    // Pre-populate skills from the initial working directory
    refresh_skills(engine, &mut input_state);

    let mut crossterm_events = EventStream::new();
    let tick_rate = Duration::from_millis(100);
    let mut tick_interval = tokio::time::interval(tick_rate);

    loop {
        // Render
        terminal.draw(|frame| {
            ui::render(frame, engine, &mut input_state);
        })?;

        // Multiplex event sources
        tokio::select! {
            // Terminal input events
            maybe_event = crossterm_events.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        match handle_terminal_event(event, engine, &mut input_state) {
                            KeyAction::Quit => return Ok(()),
                            KeyAction::SwitchModel(query) => {
                                match engine.prepare_model_switch(&query) {
                                    Ok(()) => {
                                        if let Err(e) = engine.init_conversation().await {
                                            engine.add_system_message(
                                                format!("Failed to initialize: {}", e),
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        engine.add_system_message(e.to_string());
                                    }
                                }
                            }
                            KeyAction::OpenModelPicker => {
                                engine.open_model_picker();
                            }
                            KeyAction::OpenToolPicker => {
                                engine.open_tool_picker();
                            }
                            KeyAction::ToggleTool(name) => {
                                if engine.toggle_tool_by_name(&name)
                                    && let Err(e) = engine.init_conversation().await
                                {
                                    engine.add_system_message(
                                        format!("Failed to initialize: {}", e),
                                    );
                                }
                            }
                            KeyAction::ApplyToolChanges => {
                                engine.apply_tool_picker();
                                if let Err(e) = engine.init_conversation().await {
                                    engine.add_system_message(
                                        format!("Failed to initialize: {}", e),
                                    );
                                }
                            }
                            KeyAction::AddDirectory(directory) => {
                                match engine.add_allowed_directory(&directory) {
                                    Ok(_) => {
                                        if let Err(e) = engine.init_conversation().await {
                                            engine.add_system_message(
                                                format!("Failed to initialize: {}", e),
                                            );
                                        }
                                    }
                                    Err(e) => engine.add_system_message(e.to_string()),
                                }
                            }
                            KeyAction::LaunchAgent(prompt) => {
                                if let Err(e) = engine.launch_sub_agent(&prompt) {
                                    engine.add_system_message(e.to_string());
                                }
                            }
                            KeyAction::ClearConversation => {
                                engine.clear_conversation();
                                if let Err(e) = engine.init_conversation().await {
                                    engine.add_system_message(
                                        format!("Failed to initialize: {}", e),
                                    );
                                }
                            }
                            KeyAction::CompactConversation => {
                                if let Err(e) = engine.compact_conversation().await {
                                    engine.add_system_message(e.to_string());
                                }
                            }
                            KeyAction::ShowContext => {
                                engine.add_system_message(engine.context_summary());
                            }
                            KeyAction::CopyLastResponse => {
                                if let Err(e) = engine.copy_last_response_to_clipboard() {
                                    engine.add_system_message(e.to_string());
                                }
                            }
                            KeyAction::ShowWorkingDirectory => {
                                let cwd = engine.current_working_directory();
                                engine.add_system_message(format!("Working directory: {}", cwd));
                            }
                            KeyAction::ChangeWorkingDirectory(directory) => {
                                match engine.set_working_directory(&directory) {
                                    Ok(_) => {
                                        if let Err(e) = engine.init_conversation().await {
                                            engine.add_system_message(
                                                format!("Failed to initialize: {}", e),
                                            );
                                        }
                                        // Refresh skills and invalidate @ file cache for the new dir
                                        refresh_skills(engine, &mut input_state);
                                        input_state.invalidate_at_files();
                                    }
                                    Err(e) => engine.add_system_message(e.to_string()),
                                }
                            }
                            KeyAction::None => {}
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!(error = ?e, "Terminal event error");
                    }
                    None => {
                        // Event stream closed
                        return Ok(());
                    }
                }
            }
            // Async app events (streaming, lifecycle)
            Some(event) = event_rx.recv() => {
                if matches!(engine.handle_event(event), EngineAction::Quit) {
                    return Ok(());
                }
            }
            // Tick for animations (streaming cursor blink)
            _ = tick_interval.tick() => {
                // Just redraw on tick for animations
            }
        }
    }
}

enum KeyAction {
    None,
    Quit,
    SwitchModel(String),
    OpenModelPicker,
    OpenToolPicker,
    ToggleTool(String),
    ApplyToolChanges,
    AddDirectory(String),
    LaunchAgent(String),
    ClearConversation,
    CompactConversation,
    ShowContext,
    CopyLastResponse,
    ShowWorkingDirectory,
    ChangeWorkingDirectory(String),
}

fn handle_terminal_event(
    event: Event,
    engine: &mut ChatEngine,
    input_state: &mut InputState,
) -> KeyAction {
    match event {
        Event::Key(key) => handle_key_event(key, engine, input_state),
        Event::Resize(_, _) => KeyAction::None,
        _ => KeyAction::None,
    }
}

fn handle_key_event(
    key: KeyEvent,
    engine: &mut ChatEngine,
    input_state: &mut InputState,
) -> KeyAction {
    // Model picker is open — handle picker-specific keys
    if let Some(ref mut picker) = engine.model_picker {
        match key.code {
            KeyCode::Up => {
                picker.move_up();
                return KeyAction::None;
            }
            KeyCode::Down => {
                picker.move_down();
                return KeyAction::None;
            }
            KeyCode::Enter => {
                let selected_id = picker.selected_id().map(|s| s.to_string());
                engine.close_model_picker();
                if let Some(id) = selected_id {
                    return KeyAction::SwitchModel(id);
                }
                return KeyAction::None;
            }
            KeyCode::Esc => {
                engine.close_model_picker();
                return KeyAction::None;
            }
            _ => return KeyAction::None,
        }
    }

    // Tool picker is open — handle picker-specific keys
    if let Some(ref mut picker) = engine.tool_picker {
        match key.code {
            KeyCode::Up => {
                picker.move_up();
                return KeyAction::None;
            }
            KeyCode::Down => {
                picker.move_down();
                return KeyAction::None;
            }
            KeyCode::Char(' ') => {
                picker.toggle_selected();
                return KeyAction::None;
            }
            KeyCode::Enter => {
                return KeyAction::ApplyToolChanges;
            }
            KeyCode::Esc => {
                engine.close_tool_picker();
                return KeyAction::None;
            }
            _ => return KeyAction::None,
        }
    }

    // Slash command menu is open while typing `/...` in input
    if input_state.is_slash_menu_open() {
        match key.code {
            KeyCode::Up => {
                input_state.move_slash_menu_up();
                return KeyAction::None;
            }
            KeyCode::Down => {
                input_state.move_slash_menu_down();
                return KeyAction::None;
            }
            KeyCode::Tab => {
                return apply_selected_slash_command(input_state, engine);
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                return apply_selected_slash_command(input_state, engine);
            }
            _ => {}
        }
    }

    // @ mention menu is open while typing `@<query>` in input
    if input_state.is_at_menu_open() {
        match key.code {
            KeyCode::Up => {
                input_state.move_at_menu_up();
                return KeyAction::None;
            }
            KeyCode::Down => {
                input_state.move_at_menu_down();
                return KeyAction::None;
            }
            KeyCode::Tab => {
                return apply_selected_at_mention(input_state);
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                return apply_selected_at_mention(input_state);
            }
            KeyCode::Esc => {
                // Allow Esc to fall through to the textarea so the user can
                // delete the `@` query naturally (the menu will close once the
                // `@` is removed from the input).
            }
            _ => {}
        }
    }

    // If there's a pending approval, handle y/n first
    if engine.pending_approval.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                engine.approve();
                return KeyAction::None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                engine.deny();
                return KeyAction::None;
            }
            _ => return KeyAction::None,
        }
    }

    match key.code {
        // Ctrl+C: stop stream or quit
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if engine.is_streaming {
                engine.stop_stream();
                KeyAction::None
            } else {
                KeyAction::Quit
            }
        }
        // Ctrl+Q: always quit
        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Quit,

        // Scroll: PageUp/PageDown, Shift+Up/Down
        KeyCode::PageUp => {
            engine.scroll_offset = engine.scroll_offset.saturating_add(10);
            KeyAction::None
        }
        KeyCode::PageDown => {
            engine.scroll_offset = engine.scroll_offset.saturating_sub(10);
            KeyAction::None
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            engine.scroll_offset = engine.scroll_offset.saturating_add(1);
            KeyAction::None
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            engine.scroll_offset = engine.scroll_offset.saturating_sub(1);
            KeyAction::None
        }

        // Enter: send message or handle command
        KeyCode::Enter if key.modifiers.is_empty() => {
            if !input_state.is_empty() && !engine.is_streaming {
                let text = input_state.peek_input();
                // Check for slash commands
                if let Some(cmd) = engine.try_handle_command(&text) {
                    input_state.take_input(); // consume the input
                    if let Some(action) = map_command_to_action(cmd, engine) {
                        return action;
                    }
                }
                let text = input_state.take_input();
                engine.send_message(text);
            }
            KeyAction::None
        }

        // All other keys: forward to textarea, then refresh @ file list if needed
        _ => {
            input_state.textarea.input(key);
            // If the input contains an @ query and we have no files yet, load them.
            // NOTE: we check has_at_query() (not is_at_menu_open()) because the menu
            // cannot be open when the file cache is empty — they depend on each other.
            if input_state.has_at_query() && input_state.at_menu_files.is_empty() {
                let cwd = engine.current_working_directory();
                input_state.ensure_at_files_loaded(std::path::Path::new(&cwd));
            }
            KeyAction::None
        }
    }
}

fn apply_selected_slash_command(
    input_state: &mut InputState,
    engine: &mut ChatEngine,
) -> KeyAction {
    let Some(item) = input_state.selected_slash_menu_item() else {
        return KeyAction::None;
    };

    input_state.set_input_text(&item.insert_text());

    if item.execute_immediately()
        && let Some(cmd) = engine.try_handle_command(&input_state.peek_input())
    {
        input_state.take_input();
        if let Some(action) = map_command_to_action(cmd, engine) {
            return action;
        }
    }

    KeyAction::None
}

fn apply_selected_at_mention(input_state: &mut InputState) -> KeyAction {
    if let Some(new_text) = input_state.apply_at_mention() {
        input_state.set_input_text(&new_text);
    }
    KeyAction::None
}

fn map_command_to_action(cmd: Command, engine: &mut ChatEngine) -> Option<KeyAction> {
    match cmd {
        Command::Model(Some(query)) => Some(KeyAction::SwitchModel(query)),
        Command::Model(None) => Some(KeyAction::OpenModelPicker),
        Command::Tools(Some(name)) => Some(KeyAction::ToggleTool(name)),
        Command::Tools(None) => Some(KeyAction::OpenToolPicker),
        Command::AddDir(Some(directory)) => Some(KeyAction::AddDirectory(directory)),
        Command::AddDir(None) => {
            engine.add_system_message("Usage: /add-dir <directory>".to_string());
            None
        }
        Command::Agent(Some(prompt)) => Some(KeyAction::LaunchAgent(prompt)),
        Command::Agent(None) => {
            engine.add_system_message("Usage: /agent <prompt>".to_string());
            None
        }
        Command::Clear => Some(KeyAction::ClearConversation),
        Command::Compact => Some(KeyAction::CompactConversation),
        Command::Context => Some(KeyAction::ShowContext),
        Command::Copy => Some(KeyAction::CopyLastResponse),
        Command::Cwd(Some(directory)) => Some(KeyAction::ChangeWorkingDirectory(directory)),
        Command::Cwd(None) => Some(KeyAction::ShowWorkingDirectory),
    }
}

/// Load filesystem skills for the engine's current working directory and populate
/// the input state so they appear in the `/` slash-command picker.
fn refresh_skills(engine: &ChatEngine, input_state: &mut InputState) {
    use std::path::Path;
    let workspace_dir = engine.execution_settings().workspace_dir.as_deref();
    let workspace_skills_dir = workspace_dir.map(|d| Path::new(d).join(".claude").join("skills"));
    let skills = engine
        .skill_service()
        .list_all_skills_sync(workspace_skills_dir.as_deref());
    input_state.set_available_skills(skills);
}
