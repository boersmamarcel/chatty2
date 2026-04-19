use anyhow::Result;
use tokio::sync::mpsc;

use crate::engine::ChatEngine;
use crate::events::AppEvent;

/// Run in headless mode: send a message, collect the response, print to stdout.
pub async fn run_headless(
    mut engine: ChatEngine,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    message: String,
) -> Result<()> {
    // Send message
    engine.send_message(message);

    // Collect response
    let mut response = String::new();

    while let Some(event) = event_rx.recv().await {
        match event {
            AppEvent::TextChunk(text) => {
                // Stream text chunks to stderr so parent process can show live progress.
                eprint!("{}", text);
                response.push_str(&text);
            }
            AppEvent::ToolCallStarted { ref name, .. } => {
                let name_str = name.clone();
                engine.handle_event(event);
                eprintln!("\n  \u{27f3} {}", name_str);
            }
            AppEvent::ToolCallResult { ref id, .. } => {
                let id_str = id.clone();
                engine.handle_event(event);
                if let Some(name) = engine
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|m| m.tool_calls())
                    .find(|tc| tc.id == id_str)
                    .map(|tc| tc.name.clone())
                {
                    eprintln!("  \u{2713} {}", name);
                }
            }
            AppEvent::ToolCallError { ref id, .. } => {
                let id_str = id.clone();
                engine.handle_event(event);
                if let Some(name) = engine
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|m| m.tool_calls())
                    .find(|tc| tc.id == id_str)
                    .map(|tc| tc.name.clone())
                {
                    eprintln!("  \u{2717} {}", name);
                }
            }
            AppEvent::StreamCompleted => break,
            AppEvent::StreamError(error) => {
                eprintln!("Error: {}", error);
                std::process::exit(1);
            }
            AppEvent::StreamCancelled => break,
            // Handle other events silently
            _ => {
                engine.handle_event(event);
            }
        }
    }

    // Print response to stdout
    println!("{}", response);

    Ok(())
}

/// Run in pipe mode: read stdin, send as message, print response to stdout.
pub async fn run_pipe(
    engine: ChatEngine,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() {
        eprintln!("No input provided on stdin");
        std::process::exit(1);
    }

    run_headless(engine, event_rx, input).await
}
