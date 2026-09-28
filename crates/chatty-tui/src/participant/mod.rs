//! Participant mode: this process is a worker behind the broker (AGE-301).
//!
//! `chatty-tui --participant-fd <N>` is `--headless` with three things
//! changed: the prompt arrives as a broker frame instead of `--message`, the
//! turn's events go back over the connection rather than nowhere at all,
//! and the agent's `invoke_agent` / `list_agents` reach local roles over
//! that same connection rather than loopback HTTP (ADR-0020, BI-4) — which
//! is why the worker connects before it builds its agent. The connection is the one the broker made for this process and
//! handed over on descriptor `N` (ADR-0020): the worker names nothing, and
//! learns its name from the broker's `welcome`. Everything between
//! those two ends — the session, the tools, the recovery loop — is the same
//! code `--headless` runs, which is the point: a worker is not a different
//! kind of agent.
//!
//! The loop itself, the frame ordering and the `SessionEvent` → A2A table
//! are `chatty_protocol_gateway::worker`'s, not this crate's: a microVM's
//! `chatty-server` is a worker too (AGE-307) and the parent must not be able
//! to tell the two apart. All that is left here is the hole the shared loop
//! leaves for the turn, filled with the headless runner.

pub mod broker;
#[cfg(test)]
mod broker_delegation;
#[cfg(test)]
mod budget_propagation;
#[cfg(test)]
mod call_chain;
#[cfg(test)]
mod delegation;
#[cfg(test)]
mod equivalence;
#[cfg(test)]
mod input_required_chain;
#[cfg(test)]
mod one_kind_of_agent;
#[cfg(test)]
pub(crate) mod stand_in;
#[cfg(test)]
mod swarm_forwarding;
#[cfg(test)]
pub(crate) mod swarm_kit;
#[cfg(test)]
mod swarm_trace;
#[cfg(test)]
mod team_preset;
#[cfg(test)]
mod tui_swarm;
#[cfg(test)]
mod typed_handoffs;

use anyhow::{Context, Result, bail};
use chatty_protocol_gateway::worker::{WorkerConnection, answer_clarifications, worker_card};
use std::ffi::OsString;
use std::os::fd::{FromRawFd, RawFd};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::events::AppEvent;
use crate::headless::{HeadlessRunner, run_headless};

/// Mark the descriptor named by `--participant-fd` close-on-exec, so no
/// shell or tool this worker starts inherits the broker's connection
/// (ADR-0020 invariant 2). `main`'s first statement: it reads the raw
/// arguments because nothing may run before it, parsing included.
///
/// A process without the flag is untouched. A descriptor that is not open is
/// an error: the broker did not hand one over, and there is nothing to run.
pub fn seal_participant_fd(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    let Some(fd) = participant_fd_arg(args) else {
        return Ok(());
    };
    let fd: RawFd = fd
        .parse()
        .with_context(|| format!("--participant-fd takes a descriptor number, got {fd:?}"))?;
    // SAFETY: `fcntl` on a descriptor number only reads and sets its flags.
    let sealed = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        flags != -1 && libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) != -1
    };
    if !sealed {
        bail!(
            "--participant-fd {fd} is not an open descriptor: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// The value of `--participant-fd`, as either `--participant-fd N` or
/// `--participant-fd=N`.
fn participant_fd_arg(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy();
        if arg == "--participant-fd" {
            return args.next().map(|v| v.to_string_lossy().into_owned());
        }
        if let Some(value) = arg.strip_prefix("--participant-fd=") {
            return Some(value.to_string());
        }
    }
    None
}

/// Say hello over the broker's connection on `fd` and wait for its
/// welcome. The agent is built after this, with the connection's transport.
pub async fn connect(fd: RawFd) -> Result<WorkerConnection> {
    // SAFETY: the descriptor was handed to this process by the broker for
    // exactly this, `seal_participant_fd` checked it is open, and nothing
    // else in the process takes ownership of it.
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    stream
        .set_nonblocking(true)
        .with_context(|| format!("--participant-fd {fd} is not the broker's connection"))?;
    let stream = UnixStream::from_std(stream)
        .with_context(|| format!("--participant-fd {fd} is not the broker's connection"))?;
    WorkerConnection::connect(stream, worker_card(env!("CARGO_PKG_VERSION"))).await
}

/// Run one delegated task over the welcomed `connection`, report, exit.
pub async fn run_participant(
    mut engine: HeadlessRunner,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    connection: WorkerConnection,
) -> Result<()> {
    // A desktop worker runs as whoever launched it; the task's bearer is
    // for a hosted worker (AGE-371) and is not read here.
    connection
        .serve_one_task(move |task, sink, inputs| async move {
            // The observer is dropped with the engine, which `run_headless`
            // consumes — that is what closes the shared loop's frame queue.
            engine.set_event_observer(sink);
            // A role with a handoff schema is told the schema with its
            // task, and its answer is checked against it (TD-2, AGE-693).
            let text = match task.handoff.as_ref() {
                Some(contract) => format!(
                    "{}\n\n{}",
                    task.text,
                    chatty_core::services::handoff::instruction(contract)
                ),
                None => task.text,
            };
            engine.set_handoff(task.handoff);
            // It runs under the tighter of its own budget and what its
            // caller left it (DP-3), before its clock starts.
            engine.narrow_budget(&task.budget);
            // A question this turn asks goes up the chain as
            // `input-required`; the answer comes back down here and lands on
            // the store the turn's `ask_user` is waiting on (AGE-306).
            tokio::spawn(answer_clarifications(
                inputs,
                engine.session.clarifications().clone(),
            ));
            run_headless(engine, event_rx, text).await
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn cloexec(fd: RawFd) -> bool {
        // SAFETY: reads a descriptor's flags.
        unsafe { libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0 }
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// ADR-0020 invariant 2, the worker's half: the inherited descriptor
    /// leaves `main` close-on-exec, in either spelling of the flag.
    #[test]
    fn the_participant_fd_is_sealed_close_on_exec() {
        for spelling in [
            |fd: &str| args(&["chatty-tui", "--headless", "--participant-fd", fd]),
            |fd: &str| args(&["chatty-tui", &format!("--participant-fd={fd}")]),
        ] {
            let (ours, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let fd = ours.as_raw_fd();
            // As the child inherits it: open across exec.
            // SAFETY: clears the flag on a descriptor this test owns.
            unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
            assert!(!cloexec(fd));

            seal_participant_fd(spelling(&fd.to_string())).unwrap();
            assert!(
                cloexec(fd),
                "the descriptor is close-on-exec once main starts"
            );
        }
    }

    #[test]
    fn a_process_without_the_flag_is_untouched_and_a_closed_fd_is_an_error() {
        seal_participant_fd(args(&["chatty-tui", "--headless", "-m", "hi"])).unwrap();

        // Far past any descriptor limit, so no other test can have opened it.
        let err = seal_participant_fd(args(&["chatty-tui", "--participant-fd", "2147483647"]))
            .unwrap_err();
        assert!(err.to_string().contains("not an open descriptor"), "{err}");
    }
}
