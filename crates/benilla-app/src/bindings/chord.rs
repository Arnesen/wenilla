//! The chord codec (decision 0997): Bevy's physical input ↔ the reference's canonical binding
//! strings — `[ALT-][CTRL-][SHIFT-]<TOKEN>`, where the token set is 1.12's own (`W`, `SPACE`,
//! `NUMPAD0`, `BUTTON4`, `MOUSEWHEELUP`, the bare punctuation characters). These strings are what
//! the table stores, the window displays (through the `KEY_*` GlobalStrings), and the files save —
//! matched by string equality exactly as the client matches them (decision 0585's law).
//!
//! Prefix order is ALT-CTRL-SHIFT, verified from 1.12's own capture Lua (`Blizzard_BindingUI.lua`
//! prepends SHIFT, then CTRL, then ALT) and its saved cache (`CTRL-SHIFT-PAGEDOWN`). The Super/Cmd
//! key is **not** a 1.12 binding modifier: a chord never carries it, a super-modified press never
//! matches (0585's `sup` addition), and a capture with Super held is ignored outright.

use bevy::input::mouse::MouseButton;
use bevy::prelude::KeyCode;

/// A bindable base input: a keyboard key, a mouse button, or one wheel direction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum BindKey {
    Key(KeyCode),
    Mouse(MouseButton),
    WheelUp,
    WheelDown,
}

/// One parsed binding chord: the exact modifier set + the base input. Equality is the whole
/// matching law — a press matches iff its held modifiers equal the chord's exactly.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Chord {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub key: BindKey,
}

impl Chord {
    /// Parse a canonical chord string (`"ALT-CTRL-SHIFT-F1"`, `"CTRL--"` = Ctrl+minus). Unknown
    /// base tokens return `None` — the table may hold them (a future command's key), the
    /// dispatcher just can't press them.
    pub(crate) fn parse(s: &str) -> Option<Chord> {
        let (mut alt, mut ctrl, mut shift) = (false, false, false);
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("ALT-") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("CTRL-") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("SHIFT-") {
                shift = true;
                rest = r;
            } else {
                break;
            }
        }
        Some(Chord {
            alt,
            ctrl,
            shift,
            key: token_key(rest)?,
        })
    }
}

/// Build the canonical chord string from live modifier state + a base token — the capture arm's
/// output (prefix order ALT-CTRL-SHIFT, the 1.12 canon).
pub(crate) fn chord_string(alt: bool, ctrl: bool, shift: bool, token: &str) -> String {
    let mut s = String::new();
    if alt {
        s.push_str("ALT-");
    }
    if ctrl {
        s.push_str("CTRL-");
    }
    if shift {
        s.push_str("SHIFT-");
    }
    s.push_str(token);
    s
}

/// The 1.12 token for a physical key — `None` for keys the reference has no name for (they are
/// ignored for binding, the client's own `UNKNOWN` posture). Modifier keys are deliberately
/// absent: they are chord *prefixes*, never base keys (`IsKeyPressIgnoredForBinding`).
///
/// `NumpadEnter` shares `ENTER` with the main Enter key (1.12 has one token; [`normalize_key`]
/// folds the pair for dispatch too).
pub(crate) fn key_token(k: KeyCode) -> Option<&'static str> {
    use KeyCode::*;
    Some(match k {
        KeyA => "A",
        KeyB => "B",
        KeyC => "C",
        KeyD => "D",
        KeyE => "E",
        KeyF => "F",
        KeyG => "G",
        KeyH => "H",
        KeyI => "I",
        KeyJ => "J",
        KeyK => "K",
        KeyL => "L",
        KeyM => "M",
        KeyN => "N",
        KeyO => "O",
        KeyP => "P",
        KeyQ => "Q",
        KeyR => "R",
        KeyS => "S",
        KeyT => "T",
        KeyU => "U",
        KeyV => "V",
        KeyW => "W",
        KeyX => "X",
        KeyY => "Y",
        KeyZ => "Z",
        Digit1 => "1",
        Digit2 => "2",
        Digit3 => "3",
        Digit4 => "4",
        Digit5 => "5",
        Digit6 => "6",
        Digit7 => "7",
        Digit8 => "8",
        Digit9 => "9",
        Digit0 => "0",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        Space => "SPACE",
        Tab => "TAB",
        Enter | NumpadEnter => "ENTER",
        Escape => "ESCAPE",
        Backspace => "BACKSPACE",
        Insert => "INSERT",
        Delete => "DELETE",
        Home => "HOME",
        End => "END",
        PageUp => "PAGEUP",
        PageDown => "PAGEDOWN",
        ArrowUp => "UP",
        ArrowDown => "DOWN",
        ArrowLeft => "LEFT",
        ArrowRight => "RIGHT",
        Numpad0 => "NUMPAD0",
        Numpad1 => "NUMPAD1",
        Numpad2 => "NUMPAD2",
        Numpad3 => "NUMPAD3",
        Numpad4 => "NUMPAD4",
        Numpad5 => "NUMPAD5",
        Numpad6 => "NUMPAD6",
        Numpad7 => "NUMPAD7",
        Numpad8 => "NUMPAD8",
        Numpad9 => "NUMPAD9",
        NumpadAdd => "NUMPADPLUS",
        NumpadSubtract => "NUMPADMINUS",
        NumpadDivide => "NUMPADDIVIDE",
        NumpadMultiply => "NUMPADMULTIPLY",
        NumpadDecimal => "NUMPADDECIMAL",
        NumLock => "NUMLOCK",
        PrintScreen => "PRINTSCREEN",
        ScrollLock => "SCROLLLOCK",
        Pause => "PAUSE",
        CapsLock => "CAPSLOCK",
        Minus => "-",
        Equal => "=",
        BracketLeft => "[",
        BracketRight => "]",
        Backslash => "\\",
        Semicolon => ";",
        Quote => "'",
        Comma => ",",
        Period => ".",
        Slash => "/",
        Backquote => "`",
        _ => return None,
    })
}

/// Fold key aliases that share one 1.12 token (`NumpadEnter` → `Enter`) so a chord parsed from
/// `ENTER` matches either physical key.
pub(crate) fn normalize_key(k: KeyCode) -> KeyCode {
    match k {
        KeyCode::NumpadEnter => KeyCode::Enter,
        other => other,
    }
}

/// The 1.12 token for a mouse button. BUTTON4 is winit's `Forward` and BUTTON5 `Back` —
/// deliberately keeping the thumb button that toggled autorun before 0997 (macOS winit maps
/// NSEvent buttonNumber 4 → `Forward`; see the old `player.rs` site) on the 1.12 default
/// (`BUTTON4 TOGGLEAUTORUN`), so the director's in-hand behavior survives the refactor.
pub(crate) fn mouse_token(b: MouseButton) -> Option<&'static str> {
    Some(match b {
        MouseButton::Left => "BUTTON1",
        MouseButton::Right => "BUTTON2",
        MouseButton::Middle => "BUTTON3",
        MouseButton::Forward => "BUTTON4",
        MouseButton::Back => "BUTTON5",
        MouseButton::Other(_) => return None,
    })
}

/// Token → base input (the parse side of [`key_token`]/[`mouse_token`], plus the wheel pair).
fn token_key(t: &str) -> Option<BindKey> {
    use KeyCode::*;
    if let Some(b) = match t {
        "BUTTON1" => Some(MouseButton::Left),
        "BUTTON2" => Some(MouseButton::Right),
        "BUTTON3" => Some(MouseButton::Middle),
        "BUTTON4" => Some(MouseButton::Forward),
        "BUTTON5" => Some(MouseButton::Back),
        _ => None,
    } {
        return Some(BindKey::Mouse(b));
    }
    match t {
        "MOUSEWHEELUP" => return Some(BindKey::WheelUp),
        "MOUSEWHEELDOWN" => return Some(BindKey::WheelDown),
        _ => {}
    }
    let k = match t {
        "A" => KeyA,
        "B" => KeyB,
        "C" => KeyC,
        "D" => KeyD,
        "E" => KeyE,
        "F" => KeyF,
        "G" => KeyG,
        "H" => KeyH,
        "I" => KeyI,
        "J" => KeyJ,
        "K" => KeyK,
        "L" => KeyL,
        "M" => KeyM,
        "N" => KeyN,
        "O" => KeyO,
        "P" => KeyP,
        "Q" => KeyQ,
        "R" => KeyR,
        "S" => KeyS,
        "T" => KeyT,
        "U" => KeyU,
        "V" => KeyV,
        "W" => KeyW,
        "X" => KeyX,
        "Y" => KeyY,
        "Z" => KeyZ,
        "1" => Digit1,
        "2" => Digit2,
        "3" => Digit3,
        "4" => Digit4,
        "5" => Digit5,
        "6" => Digit6,
        "7" => Digit7,
        "8" => Digit8,
        "9" => Digit9,
        "0" => Digit0,
        "F1" => F1,
        "F2" => F2,
        "F3" => F3,
        "F4" => F4,
        "F5" => F5,
        "F6" => F6,
        "F7" => F7,
        "F8" => F8,
        "F9" => F9,
        "F10" => F10,
        "F11" => F11,
        "F12" => F12,
        "SPACE" => Space,
        "TAB" => Tab,
        "ENTER" => Enter,
        "ESCAPE" => Escape,
        "BACKSPACE" => Backspace,
        "INSERT" => Insert,
        "DELETE" => Delete,
        "HOME" => Home,
        "END" => End,
        "PAGEUP" => PageUp,
        "PAGEDOWN" => PageDown,
        "UP" => ArrowUp,
        "DOWN" => ArrowDown,
        "LEFT" => ArrowLeft,
        "RIGHT" => ArrowRight,
        "NUMPAD0" => Numpad0,
        "NUMPAD1" => Numpad1,
        "NUMPAD2" => Numpad2,
        "NUMPAD3" => Numpad3,
        "NUMPAD4" => Numpad4,
        "NUMPAD5" => Numpad5,
        "NUMPAD6" => Numpad6,
        "NUMPAD7" => Numpad7,
        "NUMPAD8" => Numpad8,
        "NUMPAD9" => Numpad9,
        "NUMPADPLUS" => NumpadAdd,
        "NUMPADMINUS" => NumpadSubtract,
        "NUMPADDIVIDE" => NumpadDivide,
        "NUMPADMULTIPLY" => NumpadMultiply,
        "NUMPADDECIMAL" => NumpadDecimal,
        "NUMLOCK" => NumLock,
        "PRINTSCREEN" => PrintScreen,
        "SCROLLLOCK" => ScrollLock,
        "PAUSE" => Pause,
        "CAPSLOCK" => CapsLock,
        "-" => Minus,
        "=" => Equal,
        "[" => BracketLeft,
        "]" => BracketRight,
        "\\" => Backslash,
        ";" => Semicolon,
        "'" => Quote,
        "," => Comma,
        "." => Period,
        "/" => Slash,
        "`" => Backquote,
        _ => return None,
    };
    Some(BindKey::Key(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codec_round_trips_every_keyboard_token() {
        // Every key with a token parses back to itself (through the alias fold).
        let mut checked = 0;
        for k in [
            KeyCode::KeyW,
            KeyCode::Digit0,
            KeyCode::F11,
            KeyCode::Space,
            KeyCode::NumLock,
            KeyCode::NumpadDivide,
            KeyCode::ArrowUp,
            KeyCode::Minus,
            KeyCode::Backquote,
            KeyCode::Quote,
        ] {
            let t = key_token(k).unwrap();
            assert_eq!(
                token_key(t),
                Some(BindKey::Key(normalize_key(k))),
                "token {t}"
            );
            checked += 1;
        }
        assert_eq!(checked, 10);
        // The alias fold: both Enters share the token and the parsed key.
        assert_eq!(key_token(KeyCode::NumpadEnter), Some("ENTER"));
        assert_eq!(token_key("ENTER"), Some(BindKey::Key(KeyCode::Enter)));
    }

    #[test]
    fn chords_parse_with_the_112_prefix_order_and_punctuation_bases() {
        assert_eq!(
            Chord::parse("ALT-CTRL-SHIFT-F1"),
            Some(Chord {
                alt: true,
                ctrl: true,
                shift: true,
                key: BindKey::Key(KeyCode::F1)
            })
        );
        // `CTRL--` is Ctrl + the minus key — the prefix strip never splits the base token.
        assert_eq!(
            Chord::parse("CTRL--"),
            Some(Chord {
                alt: false,
                ctrl: true,
                shift: false,
                key: BindKey::Key(KeyCode::Minus)
            })
        );
        assert_eq!(
            Chord::parse("SHIFT-MOUSEWHEELUP"),
            Some(Chord {
                alt: false,
                ctrl: false,
                shift: true,
                key: BindKey::WheelUp
            })
        );
        assert_eq!(
            Chord::parse("BUTTON4"),
            Some(Chord {
                alt: false,
                ctrl: false,
                shift: false,
                key: BindKey::Mouse(MouseButton::Forward)
            })
        );
        assert_eq!(Chord::parse("BOGUS"), None);
        // The builder emits the same canon the parser reads.
        assert_eq!(
            chord_string(true, false, true, "PAGEDOWN"),
            "ALT-SHIFT-PAGEDOWN"
        );
        assert_eq!(
            Chord::parse("ALT-SHIFT-PAGEDOWN").unwrap(),
            Chord {
                alt: true,
                ctrl: false,
                shift: true,
                key: BindKey::Key(KeyCode::PageDown)
            }
        );
    }
}
