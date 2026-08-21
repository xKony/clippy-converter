use anyhow::{Context, Result, anyhow};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use tracing::warn;

/// Parses a hotkey string like "Shift+Alt+C" into a `HotKey`, falling back to
/// the default (`Shift+Alt+C`) with a warning when the string is invalid.
///
/// Use this on any path that registers a hotkey so registration never receives
/// an unparseable value.
#[must_use]
pub fn parse_hotkey_or_default(s: &str) -> HotKey {
    parse_hotkey(s).unwrap_or_else(|err| {
        warn!(error = %err, hotkey = %s, "invalid hotkey; falling back to default");
        HotKey::new(Some(Modifiers::SHIFT | Modifiers::ALT), Code::KeyC)
    })
}

/// Parses a hotkey string like "Shift+Alt+C" into a `HotKey`.
///
/// # Errors
/// Returns an error if the hotkey string is invalid or contains unknown keys/modifiers.
pub fn parse_hotkey(s: &str) -> Result<HotKey> {
    let s = s.trim();
    if s.is_empty() {
        return Err(anyhow!("Empty hotkey string"));
    }

    let parts: Vec<&str> = s.split('+').map(str::trim).collect();

    let mut modifiers = Modifiers::empty();
    let mut code = None;

    for part in parts {
        match part.to_lowercase().as_str() {
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" => modifiers |= Modifiers::ALT,
            "control" | "ctrl" => modifiers |= Modifiers::CONTROL,
            "meta" | "super" | "command" | "windows" => modifiers |= Modifiers::SUPER,
            key_str => {
                if code.is_some() {
                    return Err(anyhow!("Multiple keys specified in hotkey: {s}"));
                }
                code = Some(parse_code(key_str)?);
            }
        }
    }

    let code = code.context("No key specified in hotkey string")?;
    Ok(HotKey::new(Some(modifiers), code))
}

const LETTER_CODES: [Code; 26] = [
    Code::KeyA,
    Code::KeyB,
    Code::KeyC,
    Code::KeyD,
    Code::KeyE,
    Code::KeyF,
    Code::KeyG,
    Code::KeyH,
    Code::KeyI,
    Code::KeyJ,
    Code::KeyK,
    Code::KeyL,
    Code::KeyM,
    Code::KeyN,
    Code::KeyO,
    Code::KeyP,
    Code::KeyQ,
    Code::KeyR,
    Code::KeyS,
    Code::KeyT,
    Code::KeyU,
    Code::KeyV,
    Code::KeyW,
    Code::KeyX,
    Code::KeyY,
    Code::KeyZ,
];

const DIGIT_CODES: [Code; 10] = [
    Code::Digit0,
    Code::Digit1,
    Code::Digit2,
    Code::Digit3,
    Code::Digit4,
    Code::Digit5,
    Code::Digit6,
    Code::Digit7,
    Code::Digit8,
    Code::Digit9,
];

/// Maps a string to a `Code` enum variant.
fn parse_code(s: &str) -> Result<Code> {
    if let [byte] = s.as_bytes() {
        let upper = byte.to_ascii_uppercase();
        if upper.is_ascii_uppercase() {
            return Ok(LETTER_CODES[usize::from(upper - b'A')]);
        }
        if byte.is_ascii_digit() {
            return Ok(DIGIT_CODES[usize::from(*byte - b'0')]);
        }
    }

    // Handle named keys
    match s.to_lowercase().as_str() {
        "space" => Ok(Code::Space),
        "enter" | "return" => Ok(Code::Enter),
        "tab" => Ok(Code::Tab),
        "escape" | "esc" => Ok(Code::Escape),
        "backspace" => Ok(Code::Backspace),
        "delete" | "del" => Ok(Code::Delete),
        "insert" | "ins" => Ok(Code::Insert),
        "home" => Ok(Code::Home),
        "end" => Ok(Code::End),
        "pageup" | "pgup" => Ok(Code::PageUp),
        "pagedown" | "pgdn" => Ok(Code::PageDown),
        "up" => Ok(Code::ArrowUp),
        "down" => Ok(Code::ArrowDown),
        "left" => Ok(Code::ArrowLeft),
        "right" => Ok(Code::ArrowRight),
        _ => Err(anyhow!("Unknown key: {s}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn test_parse_hotkey() {
        let hk = parse_hotkey("Shift+Alt+C").unwrap();
        assert_eq!(hk.mods, Modifiers::SHIFT | Modifiers::ALT);
        assert_eq!(hk.key, Code::KeyC);

        let hk = parse_hotkey("Ctrl+Space").unwrap();
        assert_eq!(hk.mods, Modifiers::CONTROL);
        assert_eq!(hk.key, Code::Space);

        let hk = parse_hotkey("Alt+1").unwrap();
        assert_eq!(hk.mods, Modifiers::ALT);
        assert_eq!(hk.key, Code::Digit1);

        let hk = parse_hotkey("shift+a").unwrap();
        assert_eq!(hk.mods, Modifiers::SHIFT);
        assert_eq!(hk.key, Code::KeyA);
    }

    #[test]
    fn test_parse_hotkey_errors() {
        assert!(parse_hotkey("Shift+").is_err());
        assert!(parse_hotkey("UnknownKey").is_err());
        assert!(parse_hotkey("Shift+Alt+C+D").is_err());
    }
}
