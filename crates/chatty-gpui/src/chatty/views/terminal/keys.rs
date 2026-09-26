//! Keyboard, paste and mouse encoding for the embedded terminal (T3).
//!
//! Everything here is a pure function of a gpui [`Keystroke`] (or a mouse
//! event's cell and button) and the terminal's [`TermMode`], so the tables
//! are unit-tested without a window. The sequences follow xterm's
//! *ctlseqs* ("XTerm Control Sequences", Thomas Dickey), sections
//! "PC-Style Function Keys", "Bracketed Paste Mode" and "Mouse Tracking";
//! nothing is taken from another terminal's source.
//!
//! What a key does when the terminal has focus:
//!
//! 1. [`is_reserved`]: one of [`RESERVED_KEYS`] → the app's keybinding.
//! 2. [`encode_key`] gives bytes → they go to the shell, and the app never
//!    sees the key.
//! 3. A Ctrl or Alt chord with no encoding (Ctrl+1, Ctrl+,) is swallowed,
//!    so it cannot trigger an app shortcut either ([`swallows`]).
//! 4. Anything else (plain and shifted printable keys) continues to gpui's
//!    text input path, which is how IME commits and dead-key compositions
//!    arrive (`EntityInputHandler::replace_text_in_range`).
//!
//! Application keypad mode (`APP_KEYPAD`) is not honoured: gpui reports a
//! keypad digit or operator as the same key as the main-row one, so the
//! difference is not visible here. Keypad Enter is plain Enter.

use chatty_terminal::alacritty_terminal::term::TermMode;
use gpui::{Keystroke, Modifiers};

/// Keys the app keeps while a terminal has focus, in gpui keystroke syntax.
/// Every other key goes to the shell. This is the single list: the dock
/// toggle and new-terminal bindings (T4) and copy/paste bind these same
/// strings.
///
/// * `ctrl-j` / `cmd-j`, `` ctrl-` ``: show or hide the terminal dock (T4).
/// * `` ctrl-shift-` ``: new terminal (T4).
/// * `ctrl-shift-c` / `ctrl-shift-v` (`cmd-c` / `cmd-v` on macOS): copy the
///   selection, paste.
/// * `ctrl-q` / `cmd-q`: quit.
///
/// On macOS every other `cmd-` chord also stays with the app, as in any
/// macOS terminal: the Command key has no terminal encoding.
#[cfg(target_os = "macos")]
pub const RESERVED_KEYS: &[&str] = &["cmd-j", "ctrl-`", "ctrl-shift-`", "cmd-c", "cmd-v", "cmd-q"];

/// Keys the app keeps while a terminal has focus; see the macOS variant.
#[cfg(not(target_os = "macos"))]
pub const RESERVED_KEYS: &[&str] = &[
    "ctrl-j",
    "ctrl-`",
    "ctrl-shift-`",
    "ctrl-shift-c",
    "ctrl-shift-v",
    "ctrl-q",
];

/// Copy the terminal selection (the reserved copy chord).
#[cfg(target_os = "macos")]
pub const COPY_KEY: &str = "cmd-c";
/// Copy the terminal selection (the reserved copy chord).
#[cfg(not(target_os = "macos"))]
pub const COPY_KEY: &str = "ctrl-shift-c";
/// Paste into the terminal (the reserved paste chord).
#[cfg(target_os = "macos")]
pub const PASTE_KEY: &str = "cmd-v";
/// Paste into the terminal (the reserved paste chord).
#[cfg(not(target_os = "macos"))]
pub const PASTE_KEY: &str = "ctrl-shift-v";

/// Whether `keystroke` is one of [`RESERVED_KEYS`].
pub fn is_reserved(keystroke: &Keystroke) -> bool {
    let (key, modifiers) = normalize(keystroke);
    RESERVED_KEYS.iter().any(|reserved| {
        Keystroke::parse(reserved).is_ok_and(|r| r.key == key && r.modifiers == modifiers)
    })
}

/// The key and modifiers with the layout's shifted symbol undone where the
/// reserved list needs it: X11 reports Ctrl+Shift+` as `ctrl-~` (shift
/// folded into the symbol).
fn normalize(keystroke: &Keystroke) -> (&str, Modifiers) {
    let mut modifiers = keystroke.modifiers;
    let key = match keystroke.key.as_str() {
        "~" => {
            modifiers.shift = true;
            "`"
        }
        key => key,
    };
    (key, modifiers)
}

/// Whether the terminal should consume `keystroke` even though
/// [`encode_key`] has nothing to send for it: a Ctrl or Alt chord that is
/// not reserved. Keeps a shell-meant chord from reaching an app shortcut.
pub fn swallows(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    !m.platform && (m.control || m.alt) && !is_reserved(keystroke)
}

/// The bytes a key sends to the child, or `None` when it has no encoding
/// here: a plain printable key (it arrives through the text input path), a
/// Command/Super chord (the app's), or a key the terminal does not know.
pub fn encode_key(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = keystroke.modifiers;
    if m.platform {
        return None;
    }
    let param = modifier_param(m);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let key = keystroke.key.as_str();

    // Cursor keys and Home/End: CSI or SS3 + final byte.
    let cursor_final = match key {
        "up" => Some(b'A'),
        "down" => Some(b'B'),
        "right" => Some(b'C'),
        "left" => Some(b'D'),
        "home" => Some(b'H'),
        "end" => Some(b'F'),
        _ => None,
    };
    if let Some(final_byte) = cursor_final {
        return Some(match param {
            Some(p) => format!("\x1b[1;{p}{}", final_byte as char).into_bytes(),
            None if app_cursor => vec![0x1b, b'O', final_byte],
            None => vec![0x1b, b'[', final_byte],
        });
    }

    // F1–F4: SS3 P..S, or CSI 1;m P..S with modifiers.
    let pf_final = match key {
        "f1" => Some(b'P'),
        "f2" => Some(b'Q'),
        "f3" => Some(b'R'),
        "f4" => Some(b'S'),
        _ => None,
    };
    if let Some(final_byte) = pf_final {
        return Some(match param {
            Some(p) => format!("\x1b[1;{p}{}", final_byte as char).into_bytes(),
            None => vec![0x1b, b'O', final_byte],
        });
    }

    // CSI n ~ keys.
    let tilde = match key {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        return Some(
            match param {
                Some(p) => format!("\x1b[{n};{p}~"),
                None => format!("\x1b[{n}~"),
            }
            .into_bytes(),
        );
    }

    // Keys that send one byte; Alt adds an ESC prefix.
    let single: Option<Vec<u8>> = match key {
        "enter" if mode.contains(TermMode::LINE_FEED_NEW_LINE) => Some(b"\r\n".to_vec()),
        "enter" => Some(b"\r".to_vec()),
        "tab" if m.shift => return Some(b"\x1b[Z".to_vec()),
        "tab" => Some(b"\t".to_vec()),
        "backspace" if m.control => Some(vec![0x08]),
        "backspace" => Some(vec![0x7f]),
        "escape" => Some(vec![0x1b]),
        _ => None,
    };
    if let Some(bytes) = single {
        return Some(alt_prefix(m.alt, bytes));
    }

    if m.control {
        return control_byte(key).map(|b| alt_prefix(m.alt, vec![b]));
    }

    if m.alt {
        let text = match (key, keystroke.key_char.as_deref()) {
            (_, Some(text)) => text,
            ("space", None) => " ",
            (key, None) if key.chars().count() == 1 => key,
            _ => return None,
        };
        return Some(alt_prefix(true, text.as_bytes().to_vec()));
    }

    None
}

/// xterm's modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4), or `None`
/// with none of them held.
fn modifier_param(m: Modifiers) -> Option<u8> {
    let bits = m.shift as u8 | (m.alt as u8) << 1 | (m.control as u8) << 2;
    (bits != 0).then_some(bits + 1)
}

fn alt_prefix(alt: bool, mut bytes: Vec<u8>) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// The C0 control code for Ctrl + `key` (xterm's defaults, including the
/// Ctrl+digit aliases), or `None` for a key with none.
fn control_byte(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = match (chars.next(), chars.next()) {
        (Some(c), None) => c,
        _ if key == "space" => return Some(0),
        _ => return None,
    };
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Bytes for pasting `text`. Line breaks become CR, as a typed Enter would
/// be. With bracketed paste on, the text is wrapped in `CSI 200~` …
/// `CSI 201~` and any ESC in it is dropped, so the text cannot end the
/// bracket early and run the rest as typed input.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return normalized.into_bytes();
    }
    let mut out = b"\x1b[200~".to_vec();
    out.extend(normalized.bytes().filter(|&b| b != 0x1b));
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// A mouse button as reported to the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held (any-motion mode, 1003).
    None,
}

impl ReportButton {
    fn code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::None => 3,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
        }
    }
}

/// What happened to the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    Press,
    Release,
    Motion,
}

/// Whether the child wants this kind of mouse event under `mode`:
/// presses and releases with any mouse mode, motion with a button held in
/// drag mode (1002), any motion in motion mode (1003).
pub fn wants_mouse(mode: TermMode, kind: ReportKind, button: ReportButton) -> bool {
    match kind {
        ReportKind::Press | ReportKind::Release => mode.intersects(TermMode::MOUSE_MODE),
        ReportKind::Motion if button == ReportButton::None => mode.contains(TermMode::MOUSE_MOTION),
        ReportKind::Motion => mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG),
    }
}

/// A mouse report for the cell at 0-based (`col`, `row`) on screen, in
/// SGR form (1006) when that mode is on, else the classic `CSI M` form
/// (UTF-8 coordinates with 1005). `None` if a classic-form coordinate is
/// too large to encode, or a wheel "release" (the wheel has none).
pub fn mouse_report(
    mode: TermMode,
    button: ReportButton,
    kind: ReportKind,
    col: usize,
    row: usize,
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    let is_wheel = matches!(button, ReportButton::WheelUp | ReportButton::WheelDown);
    if is_wheel && kind == ReportKind::Release {
        return None;
    }
    let mut code = button.code();
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    if kind == ReportKind::Motion {
        code += 32;
    }
    let (x, y) = (col + 1, row + 1);

    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if kind == ReportKind::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }

    // Classic form: a release does not say which button.
    if kind == ReportKind::Release {
        code = (code & !0b11) | 3;
    }
    let mut out = b"\x1b[M".to_vec();
    out.push(u8::try_from(32 + code).ok()?);
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    for v in [x, y] {
        let v = 32 + v as u32;
        if utf8 {
            let c = char::from_u32(v).filter(|_| v < 2048)?;
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(u8::try_from(v).ok()?);
        }
    }
    Some(out)
}

/// Bytes for one wheel notch in the alternate screen with alternate
/// scroll on (1007) and no mouse mode: the cursor key the wheel stands for,
/// so `less` and `man` scroll.
pub fn alternate_scroll(up: bool, mode: TermMode) -> Vec<u8> {
    let final_byte = if up { b'A' } else { b'B' };
    if mode.contains(TermMode::APP_CURSOR) {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(source: &str) -> Keystroke {
        Keystroke::parse(source).unwrap()
    }

    fn enc(source: &str, mode: TermMode) -> Option<Vec<u8>> {
        encode_key(&ks(source), mode)
    }

    fn normal(source: &str) -> Vec<u8> {
        enc(source, TermMode::NONE).unwrap_or_else(|| panic!("{source} has no encoding"))
    }

    #[test]
    fn cursor_keys_follow_application_cursor_mode() {
        let app = TermMode::APP_CURSOR;
        for (key, fin) in [
            ("up", 'A'),
            ("down", 'B'),
            ("right", 'C'),
            ("left", 'D'),
            ("home", 'H'),
            ("end", 'F'),
        ] {
            assert_eq!(normal(key), format!("\x1b[{fin}").as_bytes(), "{key}");
            assert_eq!(
                enc(key, app).unwrap(),
                format!("\x1bO{fin}").as_bytes(),
                "{key} app"
            );
            // With a modifier the CSI 1;m form wins in both modes.
            assert_eq!(
                enc(&format!("ctrl-{key}"), app).unwrap(),
                format!("\x1b[1;5{fin}").as_bytes()
            );
        }
    }

    #[test]
    fn xterm_modifier_parameters() {
        assert_eq!(normal("ctrl-up"), b"\x1b[1;5A");
        assert_eq!(normal("shift-up"), b"\x1b[1;2A");
        assert_eq!(normal("alt-left"), b"\x1b[1;3D");
        assert_eq!(normal("alt-shift-right"), b"\x1b[1;4C");
        assert_eq!(normal("ctrl-shift-down"), b"\x1b[1;6B");
        assert_eq!(normal("ctrl-alt-up"), b"\x1b[1;7A");
        assert_eq!(normal("ctrl-alt-shift-up"), b"\x1b[1;8A");
        assert_eq!(normal("ctrl-delete"), b"\x1b[3;5~");
        assert_eq!(normal("shift-f5"), b"\x1b[15;2~");
        assert_eq!(normal("ctrl-f1"), b"\x1b[1;5P");
    }

    #[test]
    fn function_and_editing_keys() {
        let table: &[(&str, &[u8])] = &[
            ("f1", b"\x1bOP"),
            ("f2", b"\x1bOQ"),
            ("f3", b"\x1bOR"),
            ("f4", b"\x1bOS"),
            ("f5", b"\x1b[15~"),
            ("f6", b"\x1b[17~"),
            ("f7", b"\x1b[18~"),
            ("f8", b"\x1b[19~"),
            ("f9", b"\x1b[20~"),
            ("f10", b"\x1b[21~"),
            ("f11", b"\x1b[23~"),
            ("f12", b"\x1b[24~"),
            ("insert", b"\x1b[2~"),
            ("delete", b"\x1b[3~"),
            ("pageup", b"\x1b[5~"),
            ("pagedown", b"\x1b[6~"),
            ("enter", b"\r"),
            ("tab", b"\t"),
            ("shift-tab", b"\x1b[Z"),
            ("backspace", b"\x7f"),
            ("ctrl-backspace", b"\x08"),
            ("alt-backspace", b"\x1b\x7f"),
            ("escape", b"\x1b"),
            ("alt-enter", b"\x1b\r"),
        ];
        for (key, bytes) in table {
            assert_eq!(normal(key), *bytes, "{key}");
            // None of these depend on application cursor mode.
            assert_eq!(enc(key, TermMode::APP_CURSOR).unwrap(), *bytes, "{key} app");
        }
        assert_eq!(enc("enter", TermMode::LINE_FEED_NEW_LINE).unwrap(), b"\r\n");
    }

    #[test]
    fn ctrl_letters_are_c0_codes() {
        assert_eq!(normal("ctrl-a"), [0x01]);
        assert_eq!(normal("ctrl-c"), [0x03]);
        assert_eq!(normal("ctrl-d"), [0x04]);
        assert_eq!(normal("ctrl-l"), [0x0c]);
        assert_eq!(normal("ctrl-r"), [0x12]);
        assert_eq!(normal("ctrl-w"), [0x17]);
        assert_eq!(normal("ctrl-z"), [0x1a]);
        // Ctrl+Shift+letter (not reserved) is the same code, as in xterm.
        assert_eq!(normal("ctrl-shift-a"), [0x01]);
        assert_eq!(normal("ctrl-space"), [0x00]);
        assert_eq!(normal("ctrl-2"), [0x00]);
        assert_eq!(normal("ctrl-["), [0x1b]);
        assert_eq!(normal("ctrl-\\"), [0x1c]);
        assert_eq!(normal("ctrl-]"), [0x1d]);
        assert_eq!(normal("ctrl-6"), [0x1e]);
        assert_eq!(normal("ctrl-/"), [0x1f]);
        assert_eq!(normal("ctrl-8"), [0x7f]);
        assert_eq!(enc("ctrl-1", TermMode::NONE), None);
        assert_eq!(enc("ctrl-,", TermMode::NONE), None);
    }

    #[test]
    fn alt_is_an_esc_prefix() {
        let mut alt_b = ks("alt-b");
        alt_b.key_char = Some("b".into());
        assert_eq!(encode_key(&alt_b, TermMode::NONE).unwrap(), b"\x1bb");
        let mut alt_shift_b = ks("alt-shift-b");
        alt_shift_b.key_char = Some("B".into());
        assert_eq!(encode_key(&alt_shift_b, TermMode::NONE).unwrap(), b"\x1bB");
        // Without a key_char the key name stands in.
        assert_eq!(normal("alt-."), b"\x1b.");
        assert_eq!(normal("alt-space"), b"\x1b ");
        assert_eq!(normal("ctrl-alt-a"), b"\x1b\x01");
    }

    #[test]
    fn printable_keys_are_left_to_the_text_input_path() {
        // IME commits and dead-key compositions come through
        // `replace_text_in_range`, so the key encoder must not also send them.
        for source in ["a", "shift-a", "space", "1", "-"] {
            let mut keystroke = ks(source);
            keystroke.key_char = Some(if source == "space" {
                " ".into()
            } else {
                keystroke.key.clone()
            });
            assert_eq!(encode_key(&keystroke, TermMode::NONE), None, "{source}");
            assert!(!swallows(&keystroke), "{source}");
        }
        // A dead key press (compose pending) has no key_char and no bytes.
        let dead = Keystroke {
            key: "dead_acute".into(),
            key_char: None,
            modifiers: Modifiers::default(),
        };
        assert_eq!(encode_key(&dead, TermMode::NONE), None);
        // Command / Super chords belong to the app.
        assert_eq!(enc("cmd-a", TermMode::NONE), None);
        assert!(!swallows(&ks("cmd-a")));
    }

    #[test]
    fn reserved_keys_parse_and_match() {
        for key in RESERVED_KEYS {
            let keystroke = Keystroke::parse(key).unwrap();
            assert!(is_reserved(&keystroke), "{key}");
            assert!(!swallows(&keystroke), "{key}");
        }
        assert!(RESERVED_KEYS.contains(&COPY_KEY));
        assert!(RESERVED_KEYS.contains(&PASTE_KEY));
        // X11 reports Ctrl+Shift+` as ctrl-~.
        assert!(is_reserved(&ks("ctrl-~")));
        // The shell's chords are not reserved, and the terminal keeps them.
        for key in ["ctrl-c", "ctrl-r", "ctrl-w", "ctrl-l", "ctrl-d", "ctrl-z"] {
            assert!(!is_reserved(&ks(key)), "{key}");
            assert!(swallows(&ks(key)), "{key}");
        }
    }

    /// The app's own non-macOS shortcuts (`actions.rs`) must not fire while
    /// the terminal has focus, except quit.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn app_shortcuts_go_to_the_shell() {
        for key in [
            "ctrl-,",
            "ctrl-b",
            "ctrl-n",
            "ctrl-up",
            "ctrl-down",
            "ctrl-backspace",
            "ctrl-p",
            "alt-y",
            "alt-shift-n",
            "ctrl-y",
            "ctrl-shift-n",
        ] {
            let keystroke = ks(key);
            assert!(!is_reserved(&keystroke), "{key}");
            assert!(swallows(&keystroke), "{key}");
        }
        assert!(is_reserved(&ks("ctrl-q")));
    }

    #[test]
    fn paste_brackets_and_normalizes_line_breaks() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(
            paste_bytes("echo 1\necho 2\n", true),
            b"\x1b[200~echo 1\recho 2\r\x1b[201~"
        );
        // An embedded end marker cannot close the bracket early.
        assert_eq!(
            paste_bytes("x\x1b[201~rm -rf /\n", true),
            b"\x1b[200~x[201~rm -rf /\r\x1b[201~"
        );
    }

    #[test]
    fn sgr_mouse_reports() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let none = Modifiers::default();
        let report = |b, k, col, row, m| mouse_report(mode, b, k, col, row, m).unwrap();
        assert_eq!(
            report(ReportButton::Left, ReportKind::Press, 0, 0, none),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            report(ReportButton::Left, ReportKind::Release, 9, 4, none),
            b"\x1b[<0;10;5m"
        );
        assert_eq!(
            report(
                ReportButton::Right,
                ReportKind::Press,
                2,
                3,
                Modifiers::control()
            ),
            b"\x1b[<18;3;4M"
        );
        assert_eq!(
            report(ReportButton::WheelUp, ReportKind::Press, 4, 4, none),
            b"\x1b[<64;5;5M"
        );
        assert_eq!(
            report(ReportButton::WheelDown, ReportKind::Press, 4, 4, none),
            b"\x1b[<65;5;5M"
        );
        assert_eq!(
            report(ReportButton::Left, ReportKind::Motion, 300, 1, none),
            b"\x1b[<32;301;2M"
        );
        assert_eq!(
            mouse_report(mode, ReportButton::WheelUp, ReportKind::Release, 0, 0, none),
            None
        );
    }

    #[test]
    fn classic_mouse_reports() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let none = Modifiers::default();
        assert_eq!(
            mouse_report(mode, ReportButton::Left, ReportKind::Press, 0, 0, none).unwrap(),
            [0x1b, b'[', b'M', 32, 33, 33]
        );
        // Release is button 3.
        assert_eq!(
            mouse_report(mode, ReportButton::Right, ReportKind::Release, 1, 2, none).unwrap(),
            [0x1b, b'[', b'M', 35, 34, 35]
        );
        // Beyond column 223 the classic form cannot encode the cell.
        assert_eq!(
            mouse_report(mode, ReportButton::Left, ReportKind::Press, 300, 0, none),
            None
        );
        // UTF-8 (1005) extends it.
        let utf8 = mouse_report(
            mode | TermMode::UTF8_MOUSE,
            ReportButton::Left,
            ReportKind::Press,
            300,
            0,
            none,
        )
        .unwrap();
        assert_eq!(&utf8[..4], [0x1b, b'[', b'M', 32]);
        assert_eq!(std::str::from_utf8(&utf8[4..]).unwrap(), "\u{14d}!");
    }

    #[test]
    fn mouse_modes_decide_what_is_reported() {
        let click = TermMode::MOUSE_REPORT_CLICK;
        let drag = TermMode::MOUSE_DRAG;
        let motion = TermMode::MOUSE_MOTION;
        assert!(!wants_mouse(
            TermMode::NONE,
            ReportKind::Press,
            ReportButton::Left
        ));
        assert!(wants_mouse(click, ReportKind::Press, ReportButton::Left));
        assert!(!wants_mouse(click, ReportKind::Motion, ReportButton::Left));
        assert!(wants_mouse(drag, ReportKind::Motion, ReportButton::Left));
        assert!(!wants_mouse(drag, ReportKind::Motion, ReportButton::None));
        assert!(wants_mouse(motion, ReportKind::Motion, ReportButton::None));
    }

    #[test]
    fn alternate_scroll_sends_cursor_keys() {
        assert_eq!(alternate_scroll(true, TermMode::NONE), b"\x1b[A");
        assert_eq!(alternate_scroll(false, TermMode::APP_CURSOR), b"\x1bOB");
    }
}
