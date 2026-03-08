use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use crate::engine::{ChatEngine, Command, EngineAction};
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
    let mut crossterm_events = EventStream::new();
    let tick_rate = Duration::from_millis(100);
    let mut tick_interval = tokio::time::interval(tick_rate);

    loop {
        // Render
        terminal.draw(|frame| {
            ui::render(frame, engine, &input_state);
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
                    match cmd {
                        Command::Model(Some(query)) => return KeyAction::SwitchModel(query),
                        Command::Model(None) => return KeyAction::OpenModelPicker,
                        Command::Tools(Some(name)) => return KeyAction::ToggleTool(name),
                        Command::Tools(None) => return KeyAction::OpenToolPicker,
                    }
                }
                let text = input_state.take_input();
                engine.send_message(text);
            }
            KeyAction::None
        }

        // All other keys: forward to textarea
        _ => {
            input_state.textarea.input(key);
            KeyAction::None
        }
    }
}
