//! The byte-stream tap: a PTY wrapper that shows every byte read from the
//! PTY to a callback before alacritty's parser sees it.
//!
//! alacritty's `EventLoop` is generic over the PTY (`EventedPty`) and reads
//! through `EventedReadWrite::reader()`. `Tapped` is that PTY with its reader
//! swapped for itself: its `Read` impl reads from the real PTY, hands the bytes
//! to the tap, and returns them. Registration, writes, resizes and child
//! events go straight to the real PTY, so the event loop, its polling and its
//! child-exit handling are alacritty's own, on every platform.

use std::io::{self, Read};
use std::sync::Arc;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

/// Callback that sees each chunk of raw PTY output, in order, before the
/// terminal parser does. It runs on the PTY thread: keep it cheap.
pub type ByteTap = Box<dyn FnMut(&[u8]) + Send + 'static>;

pub(crate) struct Tapped<P> {
    pty: P,
    tap: Option<ByteTap>,
}

impl<P> Tapped<P> {
    pub(crate) fn new(pty: P, tap: Option<ByteTap>) -> Self {
        Self { pty, tap }
    }
}

impl<P: EventedReadWrite> Read for Tapped<P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.pty.reader().read(buf)?;
        if n > 0
            && let Some(tap) = self.tap.as_mut()
        {
            tap(&buf[..n]);
        }
        Ok(n)
    }
}

impl<P: EventedReadWrite> EventedReadWrite for Tapped<P> {
    type Reader = Self;
    type Writer = P::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: forwarded as-is; the caller's contract (sources outlive their
        // registration) holds for the inner PTY because `self` owns it.
        unsafe { self.pty.register(poll, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.pty.reregister(poll, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self {
        self
    }

    fn writer(&mut self) -> &mut P::Writer {
        self.pty.writer()
    }
}

impl<P: EventedPty> EventedPty for Tapped<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl<P: OnResize> OnResize for Tapped<P> {
    fn on_resize(&mut self, size: WindowSize) {
        self.pty.on_resize(size);
    }
}
