//! Forwarded user input (AGE-156): mouse and keyboard events from the
//! artifact window, translated to CDP `Input.*` commands.
//!
//! Two paths for keyboard input, deliberately not one: plain typing goes
//! through `Input.insertText`, which needs no keycode table and handles
//! unicode/IME text correctly. Anything else — Enter, Backspace, arrows,
//! Ctrl/Cmd shortcuts — needs a real `keyDown`/`keyUp` pair with a DOM key
//! name, because that is what page JS listens for.

use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton as CdpMouseButton,
};
use chromiumoxide::page::Page;

use super::error::BrowserError;

/// Modifier keys held during an input event. `alt`/`ctrl`/`meta`/`shift`
/// mirror CDP's bitfield exactly (Alt=1, Ctrl=2, Meta/Command=4, Shift=8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputModifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

impl InputModifiers {
    fn bits(self) -> i64 {
        let mut bits = 0;
        if self.alt {
            bits |= 1;
        }
        if self.ctrl {
            bits |= 2;
        }
        if self.meta {
            bits |= 4;
        }
        if self.shift {
            bits |= 8;
        }
        bits
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButtonKind {
    Left,
    Right,
    Middle,
}

impl From<MouseButtonKind> for CdpMouseButton {
    fn from(value: MouseButtonKind) -> Self {
        match value {
            MouseButtonKind::Left => CdpMouseButton::Left,
            MouseButtonKind::Right => CdpMouseButton::Right,
            MouseButtonKind::Middle => CdpMouseButton::Middle,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum MouseAction {
    Move,
    Down {
        button: MouseButtonKind,
        click_count: i64,
    },
    Up {
        button: MouseButtonKind,
        click_count: i64,
    },
    Wheel {
        delta_x: f64,
        delta_y: f64,
    },
}

/// One forwarded mouse event, already in CDP viewport space (CSS pixels —
/// the coordinate mapping from artifact-window space happens on the GPUI
/// side, which is also where DPI scaling gets resolved away: both ends
/// speak device-independent pixels, so nothing here needs to know the
/// display's scale factor).
#[derive(Clone, Copy, Debug)]
pub struct MouseInput {
    pub action: MouseAction,
    pub x: f64,
    pub y: f64,
    pub modifiers: InputModifiers,
}

/// One forwarded keyboard event.
#[derive(Clone, Debug)]
pub enum KeyInput {
    /// Plain text with no Ctrl/Cmd/Alt held — `Input.insertText`.
    Text(String),
    /// A named key (Enter, Backspace, arrows, …) or a shortcut — a real
    /// `keyDown`/`keyUp` pair via [`key_mapping`].
    Special {
        name: String,
        modifiers: InputModifiers,
    },
}

pub(super) async fn dispatch_mouse(page: &Page, input: MouseInput) -> Result<(), BrowserError> {
    let modifiers = input.modifiers.bits();
    let mut builder = DispatchMouseEventParams::builder()
        .x(input.x)
        .y(input.y)
        .modifiers(modifiers);
    builder = match input.action {
        MouseAction::Move => builder.r#type(DispatchMouseEventType::MouseMoved),
        MouseAction::Down {
            button,
            click_count,
        } => builder
            .r#type(DispatchMouseEventType::MousePressed)
            .button(CdpMouseButton::from(button))
            .click_count(click_count),
        MouseAction::Up {
            button,
            click_count,
        } => builder
            .r#type(DispatchMouseEventType::MouseReleased)
            .button(CdpMouseButton::from(button))
            .click_count(click_count),
        MouseAction::Wheel { delta_x, delta_y } => builder
            .r#type(DispatchMouseEventType::MouseWheel)
            .delta_x(delta_x)
            .delta_y(delta_y),
    };
    let params = builder
        .build()
        .map_err(|e| BrowserError::Protocol(format!("invalid mouse event: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| BrowserError::Protocol(format!("dispatchMouseEvent failed: {e}")))?;
    Ok(())
}

pub(super) async fn dispatch_key(page: &Page, input: KeyInput) -> Result<(), BrowserError> {
    match input {
        KeyInput::Text(text) => {
            if text.is_empty() {
                return Ok(());
            }
            page.execute(InsertTextParams::new(text))
                .await
                .map_err(|e| BrowserError::Protocol(format!("insertText failed: {e}")))?;
            Ok(())
        }
        KeyInput::Special { name, modifiers } => {
            let Some(mapping) = key_mapping(&name) else {
                tracing::warn!(key = %name, "browser: no CDP mapping for key, dropping");
                return Ok(());
            };
            let modifier_bits = modifiers.bits();
            for kind in [
                DispatchKeyEventType::RawKeyDown,
                DispatchKeyEventType::KeyUp,
            ] {
                let params = DispatchKeyEventParams::builder()
                    .r#type(kind)
                    .key(mapping.key)
                    .code(mapping.code)
                    .windows_virtual_key_code(mapping.vk)
                    .native_virtual_key_code(mapping.vk)
                    .modifiers(modifier_bits)
                    .build()
                    .map_err(|e| BrowserError::Protocol(format!("invalid key event: {e}")))?;
                page.execute(params)
                    .await
                    .map_err(|e| BrowserError::Protocol(format!("dispatchKeyEvent failed: {e}")))?;
            }
            Ok(())
        }
    }
}

struct KeyMapping {
    key: &'static str,
    code: &'static str,
    vk: i64,
}

/// DOM `key`/`code` and Windows virtual-key code for the special keys and
/// shortcut letters/digits a human takeover plausibly sends. Plain
/// character typing never reaches this table — it goes through
/// `Input.insertText` instead, so this only needs to cover non-printable
/// keys and the letters/digits used in Ctrl/Cmd shortcuts (select-all,
/// copy, paste, …).
fn key_mapping(name: &str) -> Option<KeyMapping> {
    let mapping = match name {
        "enter" => KeyMapping {
            key: "Enter",
            code: "Enter",
            vk: 13,
        },
        "backspace" => KeyMapping {
            key: "Backspace",
            code: "Backspace",
            vk: 8,
        },
        "tab" => KeyMapping {
            key: "Tab",
            code: "Tab",
            vk: 9,
        },
        "escape" => KeyMapping {
            key: "Escape",
            code: "Escape",
            vk: 27,
        },
        "space" => KeyMapping {
            key: " ",
            code: "Space",
            vk: 32,
        },
        "up" => KeyMapping {
            key: "ArrowUp",
            code: "ArrowUp",
            vk: 38,
        },
        "down" => KeyMapping {
            key: "ArrowDown",
            code: "ArrowDown",
            vk: 40,
        },
        "left" => KeyMapping {
            key: "ArrowLeft",
            code: "ArrowLeft",
            vk: 37,
        },
        "right" => KeyMapping {
            key: "ArrowRight",
            code: "ArrowRight",
            vk: 39,
        },
        "delete" => KeyMapping {
            key: "Delete",
            code: "Delete",
            vk: 46,
        },
        "home" => KeyMapping {
            key: "Home",
            code: "Home",
            vk: 36,
        },
        "end" => KeyMapping {
            key: "End",
            code: "End",
            vk: 35,
        },
        "pageup" => KeyMapping {
            key: "PageUp",
            code: "PageUp",
            vk: 33,
        },
        "pagedown" => KeyMapping {
            key: "PageDown",
            code: "PageDown",
            vk: 34,
        },
        _ => return key_mapping_alphanumeric(name),
    };
    Some(mapping)
}

/// `a`..`z` and `0`..`9` — needed as *shortcut* keys (Ctrl+A, Ctrl+C, …).
/// Plain unmodified letters/digits are typed via `insertText` and never
/// reach here.
fn key_mapping_alphanumeric(name: &str) -> Option<KeyMapping> {
    let mut chars = name.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if ch.is_ascii_lowercase() {
        let code: &'static str = ascii_letter_code(ch)?;
        let vk = 65 + (ch as i64 - 'a' as i64);
        return Some(KeyMapping {
            key: ascii_letter_key(ch)?,
            code,
            vk,
        });
    }
    if ch.is_ascii_digit() {
        let code: &'static str = ascii_digit_code(ch)?;
        let vk = 48 + (ch as i64 - '0' as i64);
        return Some(KeyMapping {
            key: ascii_digit_key(ch)?,
            code,
            vk,
        });
    }
    None
}

macro_rules! letter_table {
    ($fn_key:ident, $fn_code:ident, $($ch:expr => $key:expr, $code:expr;)+) => {
        fn $fn_key(ch: char) -> Option<&'static str> {
            match ch { $($ch => Some($key),)+ _ => None }
        }
        fn $fn_code(ch: char) -> Option<&'static str> {
            match ch { $($ch => Some($code),)+ _ => None }
        }
    };
}

letter_table!(ascii_letter_key, ascii_letter_code,
    'a' => "a", "KeyA"; 'b' => "b", "KeyB"; 'c' => "c", "KeyC"; 'd' => "d", "KeyD";
    'e' => "e", "KeyE"; 'f' => "f", "KeyF"; 'g' => "g", "KeyG"; 'h' => "h", "KeyH";
    'i' => "i", "KeyI"; 'j' => "j", "KeyJ"; 'k' => "k", "KeyK"; 'l' => "l", "KeyL";
    'm' => "m", "KeyM"; 'n' => "n", "KeyN"; 'o' => "o", "KeyO"; 'p' => "p", "KeyP";
    'q' => "q", "KeyQ"; 'r' => "r", "KeyR"; 's' => "s", "KeyS"; 't' => "t", "KeyT";
    'u' => "u", "KeyU"; 'v' => "v", "KeyV"; 'w' => "w", "KeyW"; 'x' => "x", "KeyX";
    'y' => "y", "KeyY"; 'z' => "z", "KeyZ";
);

letter_table!(ascii_digit_key, ascii_digit_code,
    '0' => "0", "Digit0"; '1' => "1", "Digit1"; '2' => "2", "Digit2"; '3' => "3", "Digit3";
    '4' => "4", "Digit4"; '5' => "5", "Digit5"; '6' => "6", "Digit6"; '7' => "7", "Digit7";
    '8' => "8", "Digit8"; '9' => "9", "Digit9";
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_named_keys() {
        let m = key_mapping("enter").expect("enter");
        assert_eq!(m.key, "Enter");
        assert_eq!(m.code, "Enter");
        assert_eq!(m.vk, 13);
    }

    #[test]
    fn maps_shortcut_letters_and_digits() {
        let m = key_mapping("a").expect("a");
        assert_eq!((m.key, m.code, m.vk), ("a", "KeyA", 65));
        let m = key_mapping("5").expect("5");
        assert_eq!((m.key, m.code, m.vk), ("5", "Digit5", 53));
    }

    #[test]
    fn unknown_and_multi_char_names_have_no_mapping() {
        assert!(key_mapping("f13").is_none());
        assert!(key_mapping("").is_none());
        assert!(key_mapping("A").is_none());
    }

    #[test]
    fn modifier_bits_match_cdp_encoding() {
        assert_eq!(InputModifiers::default().bits(), 0);
        assert_eq!(
            InputModifiers {
                alt: true,
                ctrl: false,
                meta: false,
                shift: false,
            }
            .bits(),
            1
        );
        assert_eq!(
            InputModifiers {
                alt: false,
                ctrl: true,
                meta: true,
                shift: true,
            }
            .bits(),
            2 | 4 | 8
        );
    }
}
