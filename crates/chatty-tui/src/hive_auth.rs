//! The TUI's own Hive sign-in (AGE-2084): `chatty-tui login`, `/login`,
//! `/logout`, and the one process-wide session everything else shares.
//!
//! Never the desktop's pair: see [`chatty_core::tui_hive`].

use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chatty_core::hive::HiveSession;
use chatty_core::tui_hive;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use tokio::sync::OnceCell;

static SESSION: OnceCell<Arc<HiveSession>> = OnceCell::const_new();

/// The Hive URL: the desktop's `hive_settings`, which the TUI only reads.
async fn registry_url() -> String {
    let desktop = chatty_core::hive_settings_repository()
        .load()
        .await
        .unwrap_or_default();
    tui_hive::registry_url(&desktop).to_string()
}

/// This process's session on the TUI's own pair. One per process: two
/// sessions would spend the same refresh token twice.
pub async fn session() -> Arc<HiveSession> {
    SESSION
        .get_or_init(|| async {
            tui_hive::open_session(
                &registry_url().await,
                chatty_core::hive_tui_settings_repository(),
            )
            .await
        })
        .await
        .clone()
}

/// The session when signed in, else `None`.
pub async fn signed_in_session() -> Option<Arc<HiveSession>> {
    let session = session().await;
    tui_hive::is_signed_in(&session).then_some(session)
}

/// Prompt for email and password on the terminal (the password without
/// echo), sign in, and return a one-line outcome.
pub async fn login() -> Result<String> {
    let url = registry_url().await;
    let session = session().await;
    let (email, password) = tokio::task::spawn_blocking(move || -> Result<(String, String)> {
        let email = prompt_line("Hive email: ")?;
        let password = prompt_password("Hive password: ")?;
        Ok((email, password))
    })
    .await??;
    if email.is_empty() || password.is_empty() {
        bail!("an email and a password are both needed");
    }
    tui_hive::sign_in(&session, &url, &email, &password).await?;
    Ok(format!("Signed in to Hive at {url} as {email}."))
}

/// Sign out and revoke the TUI's refresh token. The desktop's is untouched.
pub async fn logout() -> String {
    session().await.sign_out().await;
    "Signed out of Hive in the TUI.".to_string()
}

/// `chatty-tui login`: sign in, then exit.
pub async fn run_login() -> Result<()> {
    let outcome = login().await;
    // Give the session's saver a moment to write the pair before exit.
    if outcome.is_ok() {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    println!("{}", outcome?);
    Ok(())
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("could not read from the terminal")?;
    Ok(line.trim().to_string())
}

/// Read a password without echo. A terminal is read key by key in raw mode;
/// a pipe (a script feeding the password) is read as one line.
fn prompt_password(prompt: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return prompt_line(prompt);
    }
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    enable_raw_mode().context("could not read the password from the terminal")?;
    let result = read_password_keys();
    let _ = disable_raw_mode();
    eprintln!();
    result
}

fn read_password_keys() -> Result<String> {
    let mut password = String::new();
    loop {
        let Event::Key(key) = crossterm::event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Ok(password),
            KeyCode::Backspace => {
                password.pop();
            }
            KeyCode::Char('c') | KeyCode::Char('d') if ctrl => bail!("cancelled"),
            KeyCode::Esc => bail!("cancelled"),
            KeyCode::Char(c) if !ctrl => password.push(c),
            _ => {}
        }
    }
}
