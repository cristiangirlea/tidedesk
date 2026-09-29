//! Which key to send to the host for a key event of the viewer's window.
//!
//! The host gets hardware scan codes, so that its own keyboard layout decides
//! what a key types, as if the keyboard were plugged into it. A keyboard's
//! events carry them. Events that tools make (automation, accessibility, pens
//! that write) may not:
//!
//! - A key given by its virtual-key code alone gets its scan code from
//!   Windows, which for the arrows, Home, End and the like names the number
//!   pad's key of the same meaning: a host with NumLock on would type a digit.
//! - A character given as such (as text is typed by tools) is no key at all.
//!   It is typed with the keys that make it on this computer's layout, which
//!   gives the same character where the host's layout is the same.

use std::collections::HashSet;

use tidedesk_core::protocol::InputEvent;
use winit::keyboard::{Key, NamedKey, PhysicalKey};
use winit::platform::scancode::PhysicalKeyExtScancode;

/// The scan code to send for a key: PC/AT set 1, with `0xE0` in the high
/// byte for extended keys. None for an event that is not a key's.
pub fn scancode(physical: PhysicalKey, logical: &Key) -> Option<u16> {
    /// The number pad's keys that move the caret with NumLock off: 7 8 9
    /// (Home, up, Page Up), 4 6, 1 2 3, 0 and the decimal point (Insert,
    /// Delete). With `0xE0` in front they are the keys that do so always.
    const PAD: [u16; 10] = [0x47, 0x48, 0x49, 0x4B, 0x4D, 0x4F, 0x50, 0x51, 0x52, 0x53];
    const EXTENDED: u16 = 0xE000;
    let scancode = physical.to_scancode().filter(|&scancode| scancode != 0)? as u16;
    if PAD.contains(&scancode) && navigation(logical) {
        return Some(EXTENDED | scancode);
    }
    Some(scancode)
}

/// Whether a key moves the caret rather than types: what the number pad's
/// keys do with NumLock off, and the keys between it and the letters always.
fn navigation(logical: &Key) -> bool {
    use NamedKey::*;
    let Key::Named(named) = logical else {
        return false;
    };
    let keys = [
        ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, Insert, Delete,
    ];
    keys.contains(named)
}

/// The keys to press and let go of, in this order, for the key of
/// `scancode` with Shift or AltGr held as the character needs.
fn strokes(scancode: u16, shift: bool, altgr: bool) -> Vec<InputEvent> {
    const LEFT_SHIFT: u16 = 0x2A;
    const ALTGR: u16 = 0xE038;
    let held = [(shift, LEFT_SHIFT), (altgr, ALTGR)];
    let held = || held.iter().filter(|(held, _)| *held).map(|(_, key)| *key);
    let key = |pressed| move |scancode| InputEvent::Key { scancode, pressed };
    let down = held().chain([scancode]).map(key(true));
    let up = [scancode].into_iter().chain(held()).map(key(false));
    down.chain(up).collect()
}

/// The keys that type `character` on this computer's keyboard layout, as
/// [`strokes`]; none where the layout has no key for it. Keys that are
/// `held` on the keyboard are left as they are: a Shift the user holds is
/// not let go of on the host.
#[cfg(windows)]
pub fn typed(character: char, held: &HashSet<u16>) -> Vec<InputEvent> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
    let mut keys = typed_on(character, unsafe { GetKeyboardLayout(0) });
    keys.retain(|key| !matches!(key, InputEvent::Key { scancode, .. } if held.contains(scancode)));
    keys
}

#[cfg(not(windows))]
pub fn typed(_: char, _: &HashSet<u16>) -> Vec<InputEvent> {
    Vec::new()
}

#[cfg(windows)]
fn typed_on(
    character: char,
    layout: windows::Win32::UI::Input::KeyboardAndMouse::HKL,
) -> Vec<InputEvent> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MAPVK_VK_TO_VSC_EX, MapVirtualKeyExW, VkKeyScanExW,
    };
    const SHIFT: u8 = 1;
    const CTRL_ALT: u8 = 2 | 4;
    // Of the characters that are no letters, those with keys of their own
    // on every layout. The others are what Ctrl makes with a letter.
    match character {
        '\r' | '\n' => return strokes(0x1C, false, false),
        '\t' => return strokes(0x0F, false, false),
        '\u{8}' => return strokes(0x0E, false, false),
        other if other.is_control() => return Vec::new(),
        _ => {}
    }
    // One UTF-16 unit is all a key makes.
    let Ok(unit) = u16::try_from(u32::from(character)) else {
        return Vec::new();
    };
    // The key in the low byte, what is held with it in the high one; -1 if
    // no key makes the character.
    let [key, held] = unsafe { VkKeyScanExW(unit, layout) }.to_le_bytes();
    let (shift, altgr) = (held & SHIFT != 0, held & CTRL_ALT == CTRL_ALT);
    // Ctrl or Alt alone give commands, not characters.
    if (key, held) == (0xFF, 0xFF)
        || held & !SHIFT & !CTRL_ALT != 0
        || (held & CTRL_ALT != 0) != altgr
    {
        return Vec::new();
    }
    let scancode = unsafe { MapVirtualKeyExW(u32::from(key), MAPVK_VK_TO_VSC_EX, Some(layout)) };
    match u16::try_from(scancode) {
        Ok(scancode) if scancode != 0 => strokes(scancode, shift, altgr),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, NativeKeyCode, SmolStr};

    fn key(scancode: u16, pressed: bool) -> InputEvent {
        InputEvent::Key { scancode, pressed }
    }

    fn character(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    #[test]
    fn a_keyboards_keys_keep_their_scan_codes() {
        let a = PhysicalKey::Code(KeyCode::KeyA);
        assert_eq!(scancode(a, &character("a")), Some(0x1E));
        let enter = PhysicalKey::Code(KeyCode::Enter);
        assert_eq!(scancode(enter, &Key::Named(NamedKey::Enter)), Some(0x1C));
        let right = PhysicalKey::Code(KeyCode::ArrowRight);
        let arrow = Key::Named(NamedKey::ArrowRight);
        assert_eq!(scancode(right, &arrow), Some(0xE04D));
        // The number pad with NumLock on types digits, on the host too.
        let six = PhysicalKey::Code(KeyCode::Numpad6);
        assert_eq!(scancode(six, &character("6")), Some(0x4D));
        let unknown = PhysicalKey::Unidentified(NativeKeyCode::Windows(0x5A));
        assert_eq!(scancode(unknown, &Key::Named(NamedKey::F24)), Some(0x5A));
    }

    /// An arrow given by its virtual-key code comes as the number pad's key
    /// that means the same with NumLock off. The host may have NumLock on.
    #[test]
    fn keys_that_move_the_caret_are_sent_as_such() {
        use KeyCode as Pad;
        use NamedKey as Means;
        for (pad, means, sent) in [
            (Pad::Numpad8, Means::ArrowUp, 0xE048),
            (Pad::Numpad2, Means::ArrowDown, 0xE050),
            (Pad::Numpad4, Means::ArrowLeft, 0xE04B),
            (Pad::Numpad6, Means::ArrowRight, 0xE04D),
            (Pad::Numpad7, Means::Home, 0xE047),
            (Pad::Numpad1, Means::End, 0xE04F),
            (Pad::Numpad9, Means::PageUp, 0xE049),
            (Pad::Numpad3, Means::PageDown, 0xE051),
            (Pad::Numpad0, Means::Insert, 0xE052),
            (Pad::NumpadDecimal, Means::Delete, 0xE053),
        ] {
            let sends = scancode(PhysicalKey::Code(pad), &Key::Named(means));
            assert_eq!(sends, Some(sent), "{means:?}");
        }
        // Not the other keys of the number pad, whatever they are said to mean.
        for (pad, sent) in [
            (Pad::NumpadSubtract, 0x4A),
            (Pad::Numpad5, 0x4C),
            (Pad::NumpadAdd, 0x4E),
        ] {
            let sends = scancode(PhysicalKey::Code(pad), &Key::Named(Means::ArrowUp));
            assert_eq!(sends, Some(sent), "{pad:?}");
        }
    }

    /// A character typed by a tool comes as a key without a scan code, which
    /// the host can do nothing with.
    #[test]
    fn an_event_without_a_key_has_no_scan_code() {
        let none = PhysicalKey::Unidentified(NativeKeyCode::Windows(0));
        assert_eq!(scancode(none, &Key::Named(NamedKey::Enter)), None);
    }

    #[test]
    fn a_character_is_typed_with_the_keys_that_make_it() {
        assert_eq!(
            strokes(0x1E, false, false),
            [key(0x1E, true), key(0x1E, false)]
        );
        let shifted = [
            key(0x2A, true),
            key(0x1E, true),
            key(0x1E, false),
            key(0x2A, false),
        ];
        assert_eq!(strokes(0x1E, true, false), shifted);
        let with_altgr = [
            key(0xE038, true),
            key(0x12, true),
            key(0x12, false),
            key(0xE038, false),
        ];
        assert_eq!(strokes(0x12, false, true), with_altgr);
    }

    #[cfg(windows)]
    #[test]
    fn characters_are_looked_up_on_the_keyboard_layout() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{KLF_NOTELLSHELL, LoadKeyboardLayoutW};
        use windows::core::w;
        // The United States' layout, which every Windows has.
        let us = unsafe { LoadKeyboardLayoutW(w!("00000409"), KLF_NOTELLSHELL) }.unwrap();
        assert_eq!(typed_on('a', us), strokes(0x1E, false, false));
        assert_eq!(typed_on('A', us), strokes(0x1E, true, false));
        assert_eq!(typed_on('1', us), strokes(0x02, false, false));
        assert_eq!(typed_on('!', us), strokes(0x02, true, false));
        assert_eq!(typed_on(' ', us), strokes(0x39, false, false));
        // No key makes these there.
        assert_eq!(typed_on('é', us), []);
        assert_eq!(typed_on('中', us), []);
        assert_eq!(typed_on('😀', us), []);
        // Nor is Ctrl+C a way to type.
        assert_eq!(typed_on('\u{3}', us), []);
        // The end of a line, a tab and a step back have their keys.
        assert_eq!(typed_on('\r', us), strokes(0x1C, false, false));
        assert_eq!(typed_on('\n', us), strokes(0x1C, false, false));
        assert_eq!(typed_on('\t', us), strokes(0x0F, false, false));
        assert_eq!(typed_on('\u{8}', us), strokes(0x0E, false, false));
    }
}
