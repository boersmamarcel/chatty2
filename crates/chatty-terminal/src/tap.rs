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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Reader that yields one queued result per `read` call; an empty queue is EOF.
    struct FakeReader {
        script: VecDeque<io::Result<Vec<u8>>>,
    }

    impl Read for FakeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.script.pop_front() {
                None => Ok(0),
                Some(Err(e)) => Err(e),
                Some(Ok(chunk)) => {
                    assert!(chunk.len() <= buf.len(), "test chunk larger than buffer");
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
            }
        }
    }

    struct FakePty {
        reader: FakeReader,
        writer: Vec<u8>,
    }

    impl FakePty {
        fn new(script: Vec<io::Result<Vec<u8>>>) -> Self {
            Self {
                reader: FakeReader {
                    script: script.into(),
                },
                writer: Vec::new(),
            }
        }
    }

    impl EventedReadWrite for FakePty {
        type Reader = FakeReader;
        type Writer = Vec<u8>;

        unsafe fn register(&mut self, _: &Arc<Poller>, _: Event, _: PollMode) -> io::Result<()> {
            Ok(())
        }

        fn reregister(&mut self, _: &Arc<Poller>, _: Event, _: PollMode) -> io::Result<()> {
            Ok(())
        }

        fn deregister(&mut self, _: &Arc<Poller>) -> io::Result<()> {
            Ok(())
        }

        fn reader(&mut self) -> &mut FakeReader {
            &mut self.reader
        }

        fn writer(&mut self) -> &mut Vec<u8> {
            &mut self.writer
        }
    }

    type Seen = Arc<Mutex<Vec<u8>>>;
    type Calls = Arc<Mutex<usize>>;

    /// A tap that records every chunk's bytes and the number of calls.
    fn recording_tap() -> (ByteTap, Seen, Calls) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(Mutex::new(0usize));
        let (s, c) = (Arc::clone(&seen), Arc::clone(&calls));
        let tap: ByteTap = Box::new(move |bytes| {
            s.lock().unwrap().extend_from_slice(bytes);
            *c.lock().unwrap() += 1;
        });
        (tap, seen, calls)
    }

    #[test]
    fn tap_sees_the_bytes_read_in_order() {
        let (tap, seen, calls) = recording_tap();
        let fake = FakePty::new(vec![
            Ok(b"hello".to_vec()),
            Ok(b", ".to_vec()),
            Ok(b"world".to_vec()),
        ]);
        let mut tapped = Tapped::new(fake, Some(tap));

        // The buffer is larger than every read, so a tap handed the whole
        // buffer instead of the bytes read would show up.
        let mut buf = [0xAAu8; 16];
        let mut out = Vec::new();
        for expected in [&b"hello"[..], &b", "[..], &b"world"[..]] {
            let n = tapped.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], expected);
            out.extend_from_slice(&buf[..n]);
        }

        assert_eq!(out, b"hello, world");
        assert_eq!(*seen.lock().unwrap(), b"hello, world");
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[test]
    fn eof_does_not_call_the_tap() {
        let (tap, seen, calls) = recording_tap();
        let fake = FakePty::new(vec![Ok(b"abc".to_vec()), Ok(Vec::new())]);
        let mut tapped = Tapped::new(fake, Some(tap));

        let mut buf = [0u8; 8];
        assert_eq!(tapped.read(&mut buf).unwrap(), 3);
        assert_eq!(*calls.lock().unwrap(), 1);

        // Explicit end of stream, then an exhausted script: both read 0.
        assert_eq!(tapped.read(&mut buf).unwrap(), 0);
        assert_eq!(tapped.read(&mut buf).unwrap(), 0);
        assert_eq!(*calls.lock().unwrap(), 1, "tap called on EOF");
        assert_eq!(*seen.lock().unwrap(), b"abc");
    }

    #[test]
    fn bytes_pass_through_without_a_tap() {
        let fake = FakePty::new(vec![Ok(b"xyz".to_vec()), Ok(b"12".to_vec())]);
        let mut tapped = Tapped::new(fake, None);

        let mut buf = [0u8; 8];
        let n = tapped.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"xyz");
        let n = tapped.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"12");
        assert_eq!(tapped.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn read_error_is_returned_and_the_tap_is_not_called() {
        let (tap, seen, calls) = recording_tap();
        let fake = FakePty::new(vec![Err(io::Error::other("pty gone"))]);
        let mut tapped = Tapped::new(fake, Some(tap));

        let mut buf = [0u8; 8];
        let err = tapped.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(err.to_string(), "pty gone");
        assert_eq!(*calls.lock().unwrap(), 0);
        assert!(seen.lock().unwrap().is_empty());
    }
}
