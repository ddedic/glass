//! Clients send macOS virtual keycodes (kVK_*); map them to PC scan codes, which name a
//! physical position whatever the keyboard layout. ⌘ is the Windows key, ⌥ is Alt.

/// A key as SendInput takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// A set 1 scan code; `true` for E0-prefixed (extended) keys.
    Scan(u16, bool),
    /// A virtual-key code, for keys with no stable scan code (media, Num Lock).
    Vk(u16),
}

use Key::{Scan, Vk};

const VK_VOLUME_MUTE: u16 = 0xAD;
const VK_VOLUME_DOWN: u16 = 0xAE;
const VK_VOLUME_UP: u16 = 0xAF;
const VK_NUMLOCK: u16 = 0x90;

pub const LEFT_CTRL: Key = Scan(0x1D, false);
pub const RIGHT_CTRL: Key = Scan(0x1D, true);
pub const V: Key = Scan(0x2F, false);
pub const LEFT_WIN: Key = Scan(0x5B, true);
pub const RIGHT_WIN: Key = Scan(0x5C, true);

/// Ctrl/Alt/Win can turn resolved text into shortcuts. Unicode ignores Shift, so keep
/// Shift held: releasing and pressing it per character can toggle IMEs or Sticky Keys.
pub fn is_text_modifier(key: Key) -> bool {
    matches!(key, Scan(0x1D | 0x38, _) | Scan(0x5B | 0x5C, true))
}

/// The key for a kVK code.
pub fn to_key(kvk: u16) -> Option<Key> {
    Some(match kvk {
        0x00 => Scan(0x1E, false), // A
        0x01 => Scan(0x1F, false), // S
        0x02 => Scan(0x20, false), // D
        0x03 => Scan(0x21, false), // F
        0x04 => Scan(0x23, false), // H
        0x05 => Scan(0x22, false), // G
        0x06 => Scan(0x2C, false), // Z
        0x07 => Scan(0x2D, false), // X
        0x08 => Scan(0x2E, false), // C
        0x09 => Scan(0x2F, false), // V
        0x0a => Scan(0x56, false), // ISO section → the key left of Z
        0x0b => Scan(0x30, false), // B
        0x0c => Scan(0x10, false), // Q
        0x0d => Scan(0x11, false), // W
        0x0e => Scan(0x12, false), // E
        0x0f => Scan(0x13, false), // R
        0x10 => Scan(0x15, false), // Y
        0x11 => Scan(0x14, false), // T
        0x12 => Scan(0x02, false), // 1
        0x13 => Scan(0x03, false), // 2
        0x14 => Scan(0x04, false), // 3
        0x15 => Scan(0x05, false), // 4
        0x16 => Scan(0x07, false), // 6
        0x17 => Scan(0x06, false), // 5
        0x18 => Scan(0x0D, false), // =
        0x19 => Scan(0x0A, false), // 9
        0x1a => Scan(0x08, false), // 7
        0x1b => Scan(0x0C, false), // -
        0x1c => Scan(0x09, false), // 8
        0x1d => Scan(0x0B, false), // 0
        0x1e => Scan(0x1B, false), // ]
        0x1f => Scan(0x18, false), // O
        0x20 => Scan(0x16, false), // U
        0x21 => Scan(0x1A, false), // [
        0x22 => Scan(0x17, false), // I
        0x23 => Scan(0x19, false), // P
        0x24 => Scan(0x1C, false), // Return
        0x25 => Scan(0x26, false), // L
        0x26 => Scan(0x24, false), // J
        0x27 => Scan(0x28, false), // '
        0x28 => Scan(0x25, false), // K
        0x29 => Scan(0x27, false), // ;
        0x2a => Scan(0x2B, false), // \
        0x2b => Scan(0x33, false), // ,
        0x2c => Scan(0x35, false), // /
        0x2d => Scan(0x31, false), // N
        0x2e => Scan(0x32, false), // M
        0x2f => Scan(0x34, false), // .
        0x30 => Scan(0x0F, false), // Tab
        0x31 => Scan(0x39, false), // Space
        0x32 => Scan(0x29, false), // `
        0x33 => Scan(0x0E, false), // Backspace
        0x35 => Scan(0x01, false), // Escape
        0x36 => Scan(0x5C, true), // Right Command → right Windows
        0x37 => Scan(0x5B, true), // Command → Windows
        0x38 => Scan(0x2A, false), // Shift
        0x39 => Scan(0x3A, false), // Caps Lock
        0x3a => Scan(0x38, false), // Option → Alt
        0x3b => Scan(0x1D, false), // Control
        0x3c => Scan(0x36, false), // Right Shift
        0x3d => Scan(0x38, true), // Right Option → right Alt
        0x3e => Scan(0x1D, true), // Right Control
        0x40 => Scan(0x68, false), // F17
        0x41 => Scan(0x53, false), // Keypad .
        0x43 => Scan(0x37, false), // Keypad *
        0x45 => Scan(0x4E, false), // Keypad +
        0x47 => Vk(VK_NUMLOCK), // Clear → Num Lock
        0x48 => Vk(VK_VOLUME_UP), // Volume Up
        0x49 => Vk(VK_VOLUME_DOWN), // Volume Down
        0x4a => Vk(VK_VOLUME_MUTE), // Mute
        0x4b => Scan(0x35, true), // Keypad /
        0x4c => Scan(0x1C, true), // Keypad Enter
        0x4e => Scan(0x4A, false), // Keypad -
        0x4f => Scan(0x69, false), // F18
        0x50 => Scan(0x6A, false), // F19
        0x51 => Scan(0x59, false), // Keypad =
        0x52 => Scan(0x52, false), // Keypad 0
        0x53 => Scan(0x4F, false), // Keypad 1
        0x54 => Scan(0x50, false), // Keypad 2
        0x55 => Scan(0x51, false), // Keypad 3
        0x56 => Scan(0x4B, false), // Keypad 4
        0x57 => Scan(0x4C, false), // Keypad 5
        0x58 => Scan(0x4D, false), // Keypad 6
        0x59 => Scan(0x47, false), // Keypad 7
        0x5a => Scan(0x6B, false), // F20
        0x5b => Scan(0x48, false), // Keypad 8
        0x5c => Scan(0x49, false), // Keypad 9
        0x60 => Scan(0x3F, false), // F5
        0x61 => Scan(0x40, false), // F6
        0x62 => Scan(0x41, false), // F7
        0x63 => Scan(0x3D, false), // F3
        0x64 => Scan(0x42, false), // F8
        0x65 => Scan(0x43, false), // F9
        0x67 => Scan(0x57, false), // F11
        0x69 => Scan(0x64, false), // F13
        0x6a => Scan(0x67, false), // F16
        0x6b => Scan(0x65, false), // F14
        0x6d => Scan(0x44, false), // F10
        0x6f => Scan(0x58, false), // F12
        0x71 => Scan(0x66, false), // F15
        0x72 => Scan(0x52, true), // Help/Insert
        0x73 => Scan(0x47, true), // Home
        0x74 => Scan(0x49, true), // Page Up
        0x75 => Scan(0x53, true), // Forward Delete
        0x76 => Scan(0x3E, false), // F4
        0x77 => Scan(0x4F, true), // End
        0x78 => Scan(0x3C, false), // F2
        0x79 => Scan(0x51, true), // Page Down
        0x7a => Scan(0x3B, false), // F1
        0x7b => Scan(0x4B, true), // Left
        0x7c => Scan(0x4D, true), // Right
        0x7d => Scan(0x50, true), // Down
        0x7e => Scan(0x48, true), // Up
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_text_lifts_shortcut_modifiers_but_never_taps_shift() {
        // Both sides of Ctrl, Alt and Win can turn text into a shortcut.
        for code in [0x3b, 0x3e, 0x3a, 0x3d, 0x37, 0x36] {
            assert!(is_text_modifier(to_key(code).unwrap()), "{code:#x}");
        }
        // Keep both Shifts held for IMEs/Sticky Keys, as well as ordinary action keys.
        for code in [0x38, 0x3c, 0x00, 0x31, 0x24, 0x39] {
            assert!(!is_text_modifier(to_key(code).unwrap()), "{code:#x}");
        }
    }

    #[test]
    fn letters_and_modifiers() {
        assert_eq!(to_key(0x00), Some(Scan(0x1E, false))); // A
        assert_eq!(to_key(0x09), Some(V));
        assert_eq!(to_key(0x37), Some(LEFT_WIN));
        assert_eq!(to_key(0x3b), Some(LEFT_CTRL));
        assert_eq!(to_key(0x7b), Some(Scan(0x4B, true))); // Left arrow, not keypad 4
        assert_eq!(to_key(0x34), None);
        // No two kVK codes map to the same key.
        let mut seen: Vec<Key> = (0u16..=0x7e).filter_map(to_key).collect();
        let n = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), n);
    }
}
