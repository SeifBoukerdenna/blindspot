//! The `hotkey` config value, parsed into a Carbon key code and modifier mask.

/// Carbon modifier masks from `Events.h` — *not* `NSEvent.ModifierFlags`, which use different
/// bits entirely. Verified by compiling against the SDK: `cmdKey` = 256, `shiftKey` = 512.
pub const CMD: u32 = 0x100;
pub const SHIFT: u32 = 0x200;
pub const OPTION: u32 = 0x800;
pub const CONTROL: u32 = 0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub key_code: u32,
    pub modifiers: u32,
}

/// ⌘⇧Space, the binding blindspot has used since M1.
///
/// Not ⌘Space, although that is the goal. Spotlight still owns ⌘Space until the user
/// unbinds it by hand, and Carbon reports success for a chord another app already holds —
/// so a ⌘Space default would register cleanly and then never fire, taking away the only
/// working hotkey on the next launch.
pub const DEFAULT: Hotkey = Hotkey {
    key_code: 0x31,
    modifiers: CMD | SHIFT,
};

/// Key names to Carbon virtual key codes, generated from `Events.h` in the macOS SDK rather
/// than typed out, so a transcription slip cannot bind the wrong physical key.
const KEYS: &[(&str, u32)] = &[
    ("a", 0x00),      // kVK_ANSI_A
    ("b", 0x0B),      // kVK_ANSI_B
    ("c", 0x08),      // kVK_ANSI_C
    ("d", 0x02),      // kVK_ANSI_D
    ("e", 0x0E),      // kVK_ANSI_E
    ("f", 0x03),      // kVK_ANSI_F
    ("g", 0x05),      // kVK_ANSI_G
    ("h", 0x04),      // kVK_ANSI_H
    ("i", 0x22),      // kVK_ANSI_I
    ("j", 0x26),      // kVK_ANSI_J
    ("k", 0x28),      // kVK_ANSI_K
    ("l", 0x25),      // kVK_ANSI_L
    ("m", 0x2E),      // kVK_ANSI_M
    ("n", 0x2D),      // kVK_ANSI_N
    ("o", 0x1F),      // kVK_ANSI_O
    ("p", 0x23),      // kVK_ANSI_P
    ("q", 0x0C),      // kVK_ANSI_Q
    ("r", 0x0F),      // kVK_ANSI_R
    ("s", 0x01),      // kVK_ANSI_S
    ("t", 0x11),      // kVK_ANSI_T
    ("u", 0x20),      // kVK_ANSI_U
    ("v", 0x09),      // kVK_ANSI_V
    ("w", 0x0D),      // kVK_ANSI_W
    ("x", 0x07),      // kVK_ANSI_X
    ("y", 0x10),      // kVK_ANSI_Y
    ("z", 0x06),      // kVK_ANSI_Z
    ("0", 0x1D),      // kVK_ANSI_0
    ("1", 0x12),      // kVK_ANSI_1
    ("2", 0x13),      // kVK_ANSI_2
    ("3", 0x14),      // kVK_ANSI_3
    ("4", 0x15),      // kVK_ANSI_4
    ("5", 0x17),      // kVK_ANSI_5
    ("6", 0x16),      // kVK_ANSI_6
    ("7", 0x1A),      // kVK_ANSI_7
    ("8", 0x1C),      // kVK_ANSI_8
    ("9", 0x19),      // kVK_ANSI_9
    ("space", 0x31),  // kVK_Space
    ("return", 0x24), // kVK_Return
    ("enter", 0x24),  // kVK_Return
    ("tab", 0x30),    // kVK_Tab
    ("escape", 0x35), // kVK_Escape
    ("esc", 0x35),    // kVK_Escape
    (";", 0x29),      // kVK_ANSI_Semicolon
    ("'", 0x27),      // kVK_ANSI_Quote
    (",", 0x2B),      // kVK_ANSI_Comma
    (".", 0x2F),      // kVK_ANSI_Period
    ("/", 0x2C),      // kVK_ANSI_Slash
    ("\\", 0x2A),     // kVK_ANSI_Backslash
    ("`", 0x32),      // kVK_ANSI_Grave
    ("-", 0x1B),      // kVK_ANSI_Minus
    ("=", 0x18),      // kVK_ANSI_Equal
    ("[", 0x21),      // kVK_ANSI_LeftBracket
    ("]", 0x1E),      // kVK_ANSI_RightBracket
    ("f1", 0x7A),     // kVK_F1
    ("f2", 0x78),     // kVK_F2
    ("f3", 0x63),     // kVK_F3
    ("f4", 0x76),     // kVK_F4
    ("f5", 0x60),     // kVK_F5
    ("f6", 0x61),     // kVK_F6
    ("f7", 0x62),     // kVK_F7
    ("f8", 0x64),     // kVK_F8
    ("f9", 0x65),     // kVK_F9
    ("f10", 0x6D),    // kVK_F10
    ("f11", 0x67),    // kVK_F11
    ("f12", 0x6F),    // kVK_F12
];

#[derive(Debug, PartialEq, Eq)]
pub enum HotkeyError {
    Empty,
    UnknownModifier(String),
    UnknownKey(String),
    NoKey,
    MoreThanOneKey,
    /// Needs ⌘, ⌃ or ⌥. A bare key, or shift plus a key, would swallow ordinary typing
    /// system-wide — `space` alone would eat every space you type.
    NeedsModifier,
}

impl std::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "hotkey is empty"),
            Self::UnknownModifier(m) => write!(f, "unknown modifier `{m}`"),
            Self::UnknownKey(k) => write!(f, "unknown key `{k}`"),
            Self::NoKey => write!(f, "hotkey names modifiers but no key"),
            Self::MoreThanOneKey => write!(f, "hotkey names more than one key"),
            Self::NeedsModifier => {
                write!(
                    f,
                    "hotkey needs cmd, ctrl or opt, or it would capture normal typing"
                )
            }
        }
    }
}

/// Parses `"cmd+shift+space"`. Case-insensitive, order-insensitive, whitespace-tolerant.
pub fn parse(spec: &str) -> Result<Hotkey, HotkeyError> {
    let parts: Vec<String> = spec
        .split('+')
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(HotkeyError::Empty);
    }

    let mut modifiers = 0;
    let mut key = None;
    for part in &parts {
        if let Some(mask) = modifier(part) {
            modifiers |= mask;
        } else if let Some(&(_, code)) = KEYS.iter().find(|(name, _)| name == part) {
            if key.replace(code).is_some() {
                return Err(HotkeyError::MoreThanOneKey);
            }
        } else if part.chars().count() > 1 && !part.starts_with('f') {
            // A multi-letter word that is not a key is most likely a misspelt modifier.
            return Err(HotkeyError::UnknownModifier(part.clone()));
        } else {
            return Err(HotkeyError::UnknownKey(part.clone()));
        }
    }

    let key_code = key.ok_or(HotkeyError::NoKey)?;
    if modifiers & (CMD | CONTROL | OPTION) == 0 {
        return Err(HotkeyError::NeedsModifier);
    }
    Ok(Hotkey {
        key_code,
        modifiers,
    })
}

/// The inverse of [`parse`], in the spelling config.toml uses.
///
/// Exists so the settings window's recorder never has to know the chord vocabulary: Swift
/// turns an `NSEvent` into a Carbon code and mask — which is genuinely shell-side work,
/// since the two modifier encodings share no bits — and this turns that back into the one
/// canonical string. A second place that knew these names is a second place for the two to
/// drift, which `every_chord_survives_a_round_trip` is what holds shut.
///
/// `None` for a key code that is not in [`KEYS`]: a key blindspot cannot name is one it
/// could not write into config.toml either.
pub fn format(hotkey: Hotkey) -> Option<String> {
    let key = KEYS
        .iter()
        .find(|(_, code)| *code == hotkey.key_code)
        .map(|(name, _)| *name)?;
    // Cmd first, so the canonical spelling of the default reads the way CLAUDE.md writes
    // it. The order is fixed rather than as-typed: two spellings of one chord would make
    // "is this value the same as the file's" a string comparison that lies.
    let mut parts: Vec<&str> = [(CMD, "cmd"), (CONTROL, "ctrl"), (OPTION, "opt"), (SHIFT, "shift")]
        .iter()
        .filter(|(mask, _)| hotkey.modifiers & mask != 0)
        .map(|(_, name)| *name)
        .collect();
    parts.push(key);
    Some(parts.join("+"))
}

fn modifier(name: &str) -> Option<u32> {
    match name {
        "cmd" | "command" | "⌘" => Some(CMD),
        "shift" | "⇧" => Some(SHIFT),
        "opt" | "option" | "alt" | "⌥" => Some(OPTION),
        "ctrl" | "control" | "⌃" => Some(CONTROL),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chord_survives_a_round_trip() {
        // The recorder's whole safety depends on this: Swift sends a code and a mask, the
        // core spells it, and parsing that spelling must give back what was sent.
        for &(_, key_code) in KEYS {
            for bits in 0..16u32 {
                let modifiers = [CMD, SHIFT, OPTION, CONTROL]
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| bits & (1 << at) != 0)
                    .map(|(_, mask)| mask)
                    .sum::<u32>();
                // A chord with no real modifier is not a hotkey, and `parse` says so.
                if modifiers & (CMD | CONTROL | OPTION) == 0 {
                    continue;
                }
                let hotkey = Hotkey {
                    key_code,
                    modifiers,
                };
                let spelled = format(hotkey).expect("every key in KEYS has a name");
                assert_eq!(
                    parse(&spelled),
                    Ok(hotkey),
                    "{spelled:?} did not parse back to what spelled it"
                );
            }
        }
    }

    #[test]
    fn the_default_spells_the_way_the_docs_write_it() {
        assert_eq!(format(DEFAULT).as_deref(), Some("cmd+shift+space"));
    }

    #[test]
    fn a_key_with_no_name_cannot_be_spelled() {
        // 0x7F is not in `KEYS`, so there is no config.toml value that would round-trip.
        assert_eq!(
            format(Hotkey {
                key_code: 0x7F,
                modifiers: CMD
            }),
            None
        );
    }

    #[test]
    fn the_default_parses_to_itself() {
        assert_eq!(parse("cmd+shift+space"), Ok(DEFAULT));
    }

    #[test]
    fn spelling_order_case_and_spacing_do_not_matter() {
        for spec in [
            "cmd+shift+space",
            "Shift + Cmd + Space",
            "command+⇧+SPACE",
            "⌘+shift+space",
        ] {
            assert_eq!(parse(spec), Ok(DEFAULT), "{spec:?}");
        }
        assert_eq!(
            parse("ctrl+opt+k"),
            Ok(Hotkey {
                key_code: 0x28,
                modifiers: CONTROL | OPTION
            })
        );
        assert_eq!(parse("cmd+;").map(|h| h.key_code), Ok(0x29));
        assert_eq!(parse("cmd+f5").map(|h| h.key_code), Ok(0x60));
    }

    #[test]
    fn a_chord_that_would_capture_typing_is_refused() {
        assert_eq!(parse("space"), Err(HotkeyError::NeedsModifier));
        assert_eq!(parse("shift+a"), Err(HotkeyError::NeedsModifier));
    }

    #[test]
    fn mistakes_are_reported_not_guessed() {
        assert_eq!(parse(""), Err(HotkeyError::Empty));
        assert_eq!(parse("cmd+shift"), Err(HotkeyError::NoKey));
        assert_eq!(parse("cmd+a+b"), Err(HotkeyError::MoreThanOneKey));
        assert_eq!(
            parse("cmmd+space"),
            Err(HotkeyError::UnknownModifier("cmmd".into()))
        );
        assert_eq!(parse("cmd+§"), Err(HotkeyError::UnknownKey("§".into())));
    }

    #[test]
    fn every_key_name_is_unique() {
        let mut names: Vec<&str> = KEYS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
