use std::io;
use std::time::Duration;

use anyhow::Result;
use chatty_core::session::HOSTED_DISABLED;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::engine::{ChatEngine, Command, EngineAction, NavigableList};
use crate::events::AppEvent;
use crate::ui::{self, InputState};

/// Lines to scroll per mouse-wheel tick.
const MOUSE_SCROLL_LINES: u16 = 3;

/// Run the interactive TUI application
pub async fn run(
    mut engine: ChatEngine,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    let mut terminal = ratatui::init();

    // Chain a panic hook that disables mouse capture before ratatui's default
    // hook restores the terminal — otherwise a panic would leave the user's
    // terminal emulator stuck sending mouse escape sequences.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        previous_hook(info);
    }));

    // Enable mouse capture so wheel events reach us. This disables the
    // terminal's native text selection — users can hold Shift to bypass capture
    // in most modern terminals (iTerm2, Kitty, Alacritty, WezTerm, GNOME).
    if let Err(e) = execute!(io::stdout(), EnableMouseCapture) {
        tracing::warn!(error = ?e, "Failed to enable mouse capture; continuing without mouse scroll");
    }

    // Without bracketed paste, a pasted block arrives as individual key events
    // and every newline in it lands on the `Enter` arm below — pasting 45 lines
    // sent 45 messages (AGE-341).
    if let Err(e) = execute!(io::stdout(), EnableBracketedPaste) {
        tracing::warn!(error = ?e, "Failed to enable bracketed paste; long pastes will arrive as key events");
    }

    let result = run_loop(&mut terminal, &mut engine, &mut event_rx).await;

    // Best-effort teardown — ignore errors since we're restoring anyway.
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
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

    // Whether the next loop iteration needs to redraw. Starts `true` so the
    // first frame always renders; after that, only a terminal event or an
    // engine event that reports `EngineAction::Redraw` sets it again — a bare
    // tick with nothing new to show does not force a rebuild (AGE-168).
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| {
                ui::render(frame, engine, &mut input_state);
            })?;
            dirty = false;
        }

        // Multiplex event sources
        tokio::select! {
            // Terminal input events
            maybe_event = crossterm_events.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        dirty = true;
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
                            KeyAction::ShowOnlineStatus => {
                                engine.add_system_message(engine.online_status());
                            }
                            KeyAction::SetOnline(target) => {
                                if let Err(e) = engine.set_online(target).await {
                                    engine.add_system_message(e.to_string());
                                }
                            }
                            KeyAction::CopyLastResponse => {
                                if let Err(e) = engine.copy_last_response_to_clipboard() {
                                    engine.add_system_message(e.to_string());
                                }
                            }
                            KeyAction::UpdateCli => {
                                engine.update_cli_if_installed().await;
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
            // Async app events (streaming, lifecycle). Drain whatever else is
            // already queued behind this one — coalescing consecutive
            // `TextChunk`s — so a fast stream produces one redraw per drained
            // batch instead of one per chunk (AGE-168).
            Some(event) = event_rx.recv() => {
                if drain_and_coalesce_events(engine, event, event_rx) {
                    dirty = true;
                }
            }
            // Tick for animations (streaming cursor blink). Idle ticks are
            // free: `dirty` only flips back on when a terminal or engine
            // event actually changed something to show (AGE-168).
            _ = tick_interval.tick() => {}
        }
    }
}

/// Time budget for draining events already queued behind the one that woke
/// the loop. Bounds how long a single burst can hold off the terminal-input
/// and tick branches of the `select!` above (AGE-168).
const DRAIN_TIME_SLICE: Duration = Duration::from_millis(16);

/// Apply one event to the engine, returning whether it asked for a redraw.
fn apply_engine_event(engine: &mut ChatEngine, event: AppEvent) -> bool {
    matches!(engine.handle_event(event), EngineAction::Redraw)
}

/// Apply `first`, then drain whatever is already queued behind it in
/// `event_rx` — merging consecutive `TextChunk`s into one appended update —
/// until the channel is empty or the time slice runs out. Non-text events
/// are applied individually, in the order they arrived: they are never
/// reordered or merged across, so interleaved tool calls and approvals still
/// land on the transcript exactly where they did before (AGE-168).
///
/// Returns whether any applied event asked for a redraw.
fn drain_and_coalesce_events(
    engine: &mut ChatEngine,
    first: AppEvent,
    event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> bool {
    let deadline = tokio::time::Instant::now() + DRAIN_TIME_SLICE;
    let mut dirty = false;
    let mut pending_text: Option<String> = None;
    let mut next = Some(first);

    loop {
        let event = match next.take() {
            Some(event) => event,
            None => {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                match event_rx.try_recv() {
                    Ok(event) => event,
                    Err(_) => break,
                }
            }
        };

        match event {
            AppEvent::TextChunk(text) => match &mut pending_text {
                Some(buf) => buf.push_str(&text),
                None => pending_text = Some(text),
            },
            other => {
                if let Some(text) = pending_text.take() {
                    dirty |= apply_engine_event(engine, AppEvent::TextChunk(text));
                }
                dirty |= apply_engine_event(engine, other);
            }
        }
    }

    if let Some(text) = pending_text.take() {
        dirty |= apply_engine_event(engine, AppEvent::TextChunk(text));
    }

    dirty
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
    UpdateCli,
    ShowWorkingDirectory,
    ChangeWorkingDirectory(String),
    /// `/online` — where this conversation runs, and what a move would carry.
    ShowOnlineStatus,
    /// `/online <url>` takes it online; `/online off` brings it back.
    SetOnline(Option<String>),
}

fn handle_terminal_event(
    event: Event,
    engine: &mut ChatEngine,
    input_state: &mut InputState,
) -> KeyAction {
    match event {
        Event::Key(key) => handle_key_event(key, engine, input_state),
        Event::Mouse(mouse) => {
            handle_mouse_event(mouse, engine);
            KeyAction::None
        }
        Event::Paste(text) => {
            handle_paste(&text, engine, input_state);
            KeyAction::None
        }
        Event::Resize(_, _) => KeyAction::None,
        _ => KeyAction::None,
    }
}

/// Put a paste into the input box: short ones verbatim, long ones behind a
/// `[Pasted text #N +M lines]` reference the engine can expand on send.
fn handle_paste(text: &str, engine: &mut ChatEngine, input_state: &mut InputState) {
    let insert = engine.record_paste(text);
    input_state.insert_paste(&insert);
}

/// Route mouse wheel events to chat scroll when the pointer is over the chat area.
fn handle_mouse_event(mouse: MouseEvent, engine: &mut ChatEngine) {
    let over_chat = is_point_in_rect(mouse.column, mouse.row, engine.last_chat_area);
    match mouse.kind {
        MouseEventKind::ScrollUp if over_chat => engine.scroll_up(MOUSE_SCROLL_LINES),
        MouseEventKind::ScrollDown if over_chat => engine.scroll_down(MOUSE_SCROLL_LINES),
        _ => {}
    }
}

fn is_point_in_rect(x: u16, y: u16, rect: ratatui::layout::Rect) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
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

    // A pending clarification owns the keyboard until it is answered — except
    // for the Ctrl shortcuts handled below. Swallowing those would leave no way
    // to stop the stream or quit while the prompt is up, and `ask_user` blocks
    // for five minutes.
    if engine.pending_clarification.is_some() && !key.modifiers.contains(KeyModifiers::CONTROL) {
        let typing = engine
            .pending_clarification
            .as_ref()
            .is_some_and(|p| p.custom.is_some());

        if typing {
            match key.code {
                KeyCode::Enter => engine.commit_clarification_custom(),
                KeyCode::Esc => engine.cancel_clarification_custom(),
                KeyCode::Backspace => engine.pop_clarification_char(),
                // Shift is how capitals arrive, so only Alt is excluded here.
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                    engine.push_clarification_char(c)
                }
                _ => {}
            }
        } else {
            match key.code {
                // Options are shown 1-indexed.
                KeyCode::Char(c @ '1'..='9') if !key.modifiers.contains(KeyModifiers::ALT) => {
                    if let Some(ix) = c.to_digit(10) {
                        engine.answer_clarification_option(ix as usize - 1);
                    }
                }
                KeyCode::Char('t') | KeyCode::Char('T')
                    if !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    engine.start_clarification_custom()
                }
                _ => {}
            }
        }
        return KeyAction::None;
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

        // Ctrl+R: show or fold the full tool-call payloads
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            announce_verbose_tools(engine);
            KeyAction::None
        }

        // Scroll: PageUp/PageDown, Shift+Up/Down
        KeyCode::PageUp => {
            engine.scroll_up(10);
            KeyAction::None
        }
        KeyCode::PageDown => {
            engine.scroll_down(10);
            KeyAction::None
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            engine.scroll_up(1);
            KeyAction::None
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            engine.scroll_down(1);
            KeyAction::None
        }
        // End: jump back to bottom and re-pin
        KeyCode::End => {
            engine.pin_to_bottom();
            KeyAction::None
        }

        // Enter: send message or handle command
        KeyCode::Enter if key.modifiers.is_empty() => {
            if !input_state.is_empty() {
                // Commands see the paste expanded, so a pasted argument still
                // reaches the command that was asked for it.
                let text = engine.expand_pastes(&input_state.peek_input());
                // /quit and /exit work even while streaming
                if let Some(Command::Quit) = engine.try_handle_command(&text) {
                    return KeyAction::Quit;
                }
                if !engine.is_streaming {
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
            }
            KeyAction::None
        }

        // All other keys: forward to textarea, then refresh @ file list if needed
        _ => {
            input_state.textarea.input(key);
            input_state.sync_paste_ranges();
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
        Command::Modules(arg) => {
            match engine.handle_modules_command(arg.as_deref()) {
                Ok(changed) => {
                    if changed {
                        engine.spawn_init_conversation();
                    }
                }
                Err(e) => engine.add_system_message(e.to_string()),
            }
            None
        }
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
        Command::Update => Some(KeyAction::UpdateCli),
        Command::Cwd(Some(directory)) => Some(KeyAction::ChangeWorkingDirectory(directory)),
        Command::Cwd(None) => Some(KeyAction::ShowWorkingDirectory),
        // AGE-308: the move is developer-only until online mode is
        // account-scoped. The command stays in the registry so it remains
        // discoverable for developers; it refuses here.
        Command::Online(_) if !engine.execution_settings().hosted_conversations_enabled => {
            engine.add_system_message(HOSTED_DISABLED.to_string());
            None
        }
        // `/online` alone shows the table before anything leaves this machine;
        // naming a URL is the confirmation. `off` is the way back.
        Command::Online(None) => Some(KeyAction::ShowOnlineStatus),
        Command::Online(Some(target)) if target.eq_ignore_ascii_case("off") => {
            Some(KeyAction::SetOnline(None))
        }
        Command::Online(Some(url)) => Some(KeyAction::SetOnline(Some(url))),
        Command::Verbose => {
            announce_verbose_tools(engine);
            None
        }
        Command::Paste(arg) => {
            show_paste(engine, arg.as_deref());
            None
        }
        Command::Quit => Some(KeyAction::Quit),
    }
}

/// Flip tool-call detail and tell the user which mode they are now in.
fn announce_verbose_tools(engine: &mut ChatEngine) {
    let message = if engine.toggle_verbose_tools() {
        "Tool detail: full payloads. Ctrl+R or /verbose to fold them again."
    } else {
        "Tool detail: folded summaries. Ctrl+R or /verbose to show full payloads."
    };
    engine.add_system_message(message.to_string());
}

/// Print an elided paste back to the transcript, so hiding it behind a
/// reference never means losing sight of what was pasted (AGE-341).
fn show_paste(engine: &mut ChatEngine, arg: Option<&str>) {
    let message = match arg.and_then(|arg| arg.trim().trim_start_matches('#').parse::<usize>().ok())
    {
        Some(id) => match engine.paste_text(id) {
            Some(text) => format!("Pasted text #{id}:\n{text}"),
            None => format!("No paste #{id} in this session."),
        },
        None => "Usage: /paste <n>, where n is the number in [Pasted text #n].".to_string(),
    };
    engine.add_system_message(message);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ChatEngineConfig;
    use chatty_core::services::StreamSurface;
    use chatty_core::settings::models::models_store::ModelConfig;
    use chatty_core::settings::models::module_settings::ModuleSettingsModel;
    use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
    use chatty_core::settings::models::{ExecutionSettingsModel, ModelsModel};

    /// An engine with no conversation and no services — enough to route a
    /// slash command through `map_command_to_action`.
    fn test_engine(execution_settings: ExecutionSettingsModel) -> ChatEngine {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        ChatEngine::new(
            ChatEngineConfig {
                model_config: ModelConfig::new(
                    "m1".to_string(),
                    "Test Model".to_string(),
                    ProviderType::Ollama,
                    "llama3.2".to_string(),
                ),
                provider_config: ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
                execution_settings,
                module_settings: ModuleSettingsModel::default(),
                broker_port: None,
                models: ModelsModel::default(),
                providers: Vec::new(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: Vec::new(),
                remote_agents: Vec::new(),
                module_agents: Vec::new(),
                is_sub_agent: false,
                services_loaded: true,
                surface: StreamSurface::InteractiveTui,
            },
            event_tx,
        )
    }

    fn last_system_message(engine: &ChatEngine) -> String {
        engine
            .transcript
            .messages
            .last()
            .expect("the refusal is shown in the transcript")
            .text()
    }

    /// AGE-308: `/online` still parses and still lists, but by default it
    /// refuses instead of moving anything off this machine.
    #[test]
    fn online_is_refused_while_hosted_conversations_are_disabled() {
        let mut engine = test_engine(ExecutionSettingsModel::default());

        for command in [
            Command::Online(None),
            Command::Online(Some("http://localhost:8081".to_string())),
            Command::Online(Some("off".to_string())),
        ] {
            assert!(
                map_command_to_action(command, &mut engine).is_none(),
                "the default build must not act on /online"
            );
            assert_eq!(last_system_message(&engine), HOSTED_DISABLED);
        }
    }

    /// With the developer setting on, `/online` behaves exactly as it did.
    #[test]
    fn online_works_once_hosted_conversations_are_enabled() {
        let settings = ExecutionSettingsModel {
            hosted_conversations_enabled: true,
            ..Default::default()
        };
        let mut engine = test_engine(settings);

        assert!(matches!(
            map_command_to_action(Command::Online(None), &mut engine),
            Some(KeyAction::ShowOnlineStatus)
        ));
        assert!(matches!(
            map_command_to_action(
                Command::Online(Some("http://localhost:8081".to_string())),
                &mut engine
            ),
            Some(KeyAction::SetOnline(Some(_)))
        ));
        assert!(matches!(
            map_command_to_action(Command::Online(Some("off".to_string())), &mut engine),
            Some(KeyAction::SetOnline(None))
        ));
    }

    fn long_paste(lines: usize) -> String {
        (1..=lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The bug this fixes: without bracketed paste every newline in the paste
    /// arrived as a bare `Enter` and sent the message (AGE-341).
    #[test]
    fn pasting_multiline_text_fills_the_input_instead_of_sending() {
        let mut engine = test_engine(ExecutionSettingsModel::default());
        let mut input_state = InputState::new();
        let pasted = long_paste(45);

        let action =
            handle_terminal_event(Event::Paste(pasted.clone()), &mut engine, &mut input_state);

        assert!(matches!(action, KeyAction::None));
        assert!(!engine.is_streaming, "a paste must not start a turn");
        assert!(
            engine.transcript.messages.is_empty(),
            "a paste must not push a user message"
        );
        assert_eq!(input_state.peek_input(), "[Pasted text #1 +45 lines]");
        assert_eq!(engine.expand_pastes(&input_state.peek_input()), pasted);
    }

    #[test]
    fn a_short_paste_lands_in_the_input_verbatim() {
        let mut engine = test_engine(ExecutionSettingsModel::default());
        let mut input_state = InputState::new();

        handle_terminal_event(
            Event::Paste("cargo test -p chatty-tui".to_string()),
            &mut engine,
            &mut input_state,
        );

        assert_eq!(input_state.peek_input(), "cargo test -p chatty-tui");
    }

    /// A reference is only safe to show if the text behind it stays reachable.
    #[test]
    fn paste_command_prints_the_elided_text() {
        let mut engine = test_engine(ExecutionSettingsModel::default());
        let mut input_state = InputState::new();
        let pasted = long_paste(7);
        handle_terminal_event(Event::Paste(pasted.clone()), &mut engine, &mut input_state);

        map_command_to_action(Command::Paste(Some("1".to_string())), &mut engine);
        assert_eq!(
            last_system_message(&engine),
            format!("Pasted text #1:\n{pasted}")
        );

        map_command_to_action(Command::Paste(Some("99".to_string())), &mut engine);
        assert_eq!(
            last_system_message(&engine),
            "No paste #99 in this session."
        );
    }

    /// AGE-168: a fast burst of `TextChunk`s must drain and coalesce in one
    /// call instead of needing one `drain_and_coalesce_events` call (and so
    /// one `terminal.draw`) per chunk — the bounded-draw-rate acceptance
    /// criterion, exercised at the unit that the main loop calls once per
    /// `select!` iteration.
    #[test]
    fn draining_a_burst_absorbs_every_queued_chunk_in_one_call() {
        let mut engine = test_engine(ExecutionSettingsModel::default());
        engine.transcript.start_assistant();

        let (tx, mut rx) = mpsc::unbounded_channel();
        for n in 1..=49 {
            tx.send(AppEvent::TextChunk(format!("chunk{n} "))).unwrap();
        }

        let dirty =
            drain_and_coalesce_events(&mut engine, AppEvent::TextChunk("chunk0 ".into()), &mut rx);

        assert!(dirty, "text chunks must ask for a redraw");
        assert!(
            rx.try_recv().is_err(),
            "every already-queued chunk must be drained in the one call"
        );
        let expected: String = (0..=49).map(|n| format!("chunk{n} ")).collect();
        assert_eq!(
            engine.transcript.messages.last().unwrap().text(),
            expected,
            "coalescing must not change the resulting text"
        );
    }

    /// AGE-168: coalescing merges consecutive `TextChunk`s only — tool calls
    /// and approvals interleaved with text must keep their relative order and
    /// must not be merged across, so visual correctness survives the change.
    #[test]
    fn draining_preserves_order_of_interleaved_tool_and_approval_events() {
        let mut engine = test_engine(ExecutionSettingsModel::default());
        engine.transcript.start_assistant();

        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(AppEvent::ToolCallStarted {
            id: "t1".to_string(),
            name: "read_file".to_string(),
        })
        .unwrap();
        tx.send(AppEvent::TextChunk("b".to_string())).unwrap();
        tx.send(AppEvent::ApprovalRequested {
            id: "a1".to_string(),
            command: "rm -rf /tmp/x".to_string(),
            is_sandboxed: false,
        })
        .unwrap();
        tx.send(AppEvent::TextChunk("c".to_string())).unwrap();

        let dirty =
            drain_and_coalesce_events(&mut engine, AppEvent::TextChunk("a".into()), &mut rx);

        assert!(dirty);
        let last = engine.transcript.messages.last().unwrap();
        assert_eq!(
            last.blocks.len(),
            3,
            "text/tool/text — never merged across the tool call"
        );
        assert!(matches!(&last.blocks[0], crate::engine::MessageBlock::Text(t) if t == "a"));
        assert!(
            matches!(&last.blocks[1], crate::engine::MessageBlock::ToolCall(tc) if tc.id == "t1")
        );
        // "b" and "c" land in the same trailing block: the approval in between
        // does not open a new text block, so they coalesce together.
        assert!(matches!(&last.blocks[2], crate::engine::MessageBlock::Text(t) if t == "bc"));
        assert_eq!(
            engine.pending_approval.as_ref().map(|a| a.id.as_str()),
            Some("a1"),
            "the approval must still be recorded, in order, alongside the text"
        );
    }

    /// AGE-168: events the engine reports no visible change for (here,
    /// `TurnMessages`, which only updates trace bookkeeping) must not mark
    /// the loop dirty — this is what lets an idle tick skip its redraw.
    #[test]
    fn apply_engine_event_does_not_mark_dirty_for_a_no_op_event() {
        let mut engine = test_engine(ExecutionSettingsModel::default());
        engine.transcript.start_assistant();

        let dirty = apply_engine_event(&mut engine, AppEvent::TurnMessages(Vec::new()));

        assert!(
            !dirty,
            "an event with no visible effect must not force a redraw"
        );
    }
}
