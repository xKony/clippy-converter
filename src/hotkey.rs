use anyhow::{Context, Result, anyhow};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

/// Parses a hotkey string like "Shift+Alt+C" into a `HotKey`.
///
/// # Errors
/// Returns an error if the hotkey string is invalid or contains unknown keys/modifiers.
pub fn parse_hotkey(s: &str) -> Result<HotKey> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if parts.is_empty() {
        return Err(anyhow!("Empty hotkey string"));
    }

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

/// Maps a string to a `Code` enum variant.
fn parse_code(s: &str) -> Result<Code> {
    // Attempt to handle common alphanumeric keys
    if s.len() == 1 {
        let c = s
            .chars()
            .next()
            .context("Empty key string")?
            .to_ascii_uppercase();
        if c.is_ascii_alphabetic() {
            return Ok(match c {
                'A' => Code::KeyA,
                'B' => Code::KeyB,
                'C' => Code::KeyC,
                'D' => Code::KeyD,
                'E' => Code::KeyE,
                'F' => Code::KeyF,
                'G' => Code::KeyG,
                'H' => Code::KeyH,
                'I' => Code::KeyI,
                'J' => Code::KeyJ,
                'K' => Code::KeyK,
                'L' => Code::KeyL,
                'M' => Code::KeyM,
                'N' => Code::KeyN,
                'O' => Code::KeyO,
                'P' => Code::KeyP,
                'Q' => Code::KeyQ,
                'R' => Code::KeyR,
                'S' => Code::KeyS,
                'T' => Code::KeyT,
                'U' => Code::KeyU,
                'V' => Code::KeyV,
                'W' => Code::KeyW,
                'X' => Code::KeyX,
                'Y' => Code::KeyY,
                'Z' => Code::KeyZ,
                _ => unreachable!(),
            });
        }
        if c.is_ascii_digit() {
            return Ok(match c {
                '0' => Code::Digit0,
                '1' => Code::Digit1,
                '2' => Code::Digit2,
                '3' => Code::Digit3,
                '4' => Code::Digit4,
                '5' => Code::Digit5,
                '6' => Code::Digit6,
                '7' => Code::Digit7,
                '8' => Code::Digit8,
                '9' => Code::Digit9,
                _ => unreachable!(),
            });
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
    }

    #[test]
    fn test_parse_hotkey_errors() {
        assert!(parse_hotkey("Shift+").is_err());
        assert!(parse_hotkey("UnknownKey").is_err());
        assert!(parse_hotkey("Shift+Alt+C+D").is_err());
    }
}
