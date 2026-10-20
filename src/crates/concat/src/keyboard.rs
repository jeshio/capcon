// SPDX-License-Identifier: AGPL-3.0-or-later

//! Physical shortcut keys at the native event boundary. Slint otherwise uses
//! the layout's character for both typing and shortcuts, including TextInput's
//! built-in clipboard commands. Translate only shortcut events, then let the
//! usual focused widget, fixed bindings and rebindable keymap route them.

use std::collections::HashMap;

use slint::platform::WindowEvent;
use slint::winit_030::winit::event::ElementState;
use slint::winit_030::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// Per-window keyboard state; release and repeat use the translation chosen
/// at key-down even if the layout, modifiers or focused widget changes.
#[derive(Default)]
pub(crate) struct PhysicalShortcuts {
    modifiers: ModifiersState,
    held: HashMap<KeyCode, Option<&'static str>>,
}

impl PhysicalShortcuts {
    /// Native modifier state, used only to distinguish commands from typing.
    /// Slint still receives the original modifier events and applies its own
    /// Command/Control swap on Apple platforms.
    pub(crate) fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    /// A window losing focus must not retain held keys from another app.
    pub(crate) fn focus_lost(&mut self) {
        self.modifiers = ModifiersState::empty();
        self.held.clear();
    }

    /// Return true when the native event was replaced and must not also be
    /// delivered by winit. Ordinary text, dead keys and IME events retain the
    /// backend's original handling.
    pub(crate) fn key_event(
        &mut self,
        window: &slint::Window,
        physical: PhysicalKey,
        logical_is_named: bool,
        state: ElementState,
        repeat: bool,
        synthetic: bool,
    ) -> bool {
        let PhysicalKey::Code(code) = physical else {
            return false;
        };
        if synthetic {
            return false;
        }
        let text = match state {
            ElementState::Released => self.held.remove(&code).flatten(),
            ElementState::Pressed => {
                let editing = text_input_has_focus(window);
                *self.held.entry(code).or_insert_with(|| {
                    (!logical_is_named
                        && should_translate(
                            editing,
                            self.modifiers,
                            cfg!(any(target_os = "macos", target_os = "ios")),
                        ))
                    .then(|| physical_text(code, self.modifiers.shift_key()))
                    .flatten()
                })
            }
        };
        let Some(text) = text else {
            return false;
        };
        let text = text.into();
        window.dispatch_event(match (state, repeat) {
            (ElementState::Released, _) => WindowEvent::KeyReleased { text },
            (ElementState::Pressed, true) => WindowEvent::KeyPressRepeated { text },
            (ElementState::Pressed, false) => WindowEvent::KeyPressed { text },
        });
        true
    }
}

fn text_input_has_focus(window: &slint::Window) -> bool {
    use slint::private_unstable_api::re_exports::{TextInput, WindowInner};
    WindowInner::from_pub(window)
        .focus_item
        .borrow()
        .upgrade()
        .is_some_and(|item| {
            item.downcast::<TextInput>()
                .is_some_and(|input| !input.as_pin_ref().read_only())
        })
}

fn should_translate(editing: bool, modifiers: ModifiersState, apple: bool) -> bool {
    if !editing {
        return true;
    }
    // Option/dead-key input on Apple and AltGr (Ctrl+Alt) on Windows/Linux
    // produce text. They must not become ASCII or editor commands.
    if apple {
        modifiers.super_key()
    } else {
        modifiers.control_key() && !modifiers.alt_key()
    }
}

/// US key-position names keep existing preferences and menu labels valid.
/// Shift is retained for punctuation; Caps Lock never changes a command.
/// Named/navigation keys keep the backend's handling, including the numeric
/// keypad when Num Lock turns its digits into navigation keys.
fn physical_text(code: KeyCode, shift: bool) -> Option<&'static str> {
    use KeyCode::*;
    Some(match code {
        KeyA => "a",
        KeyB => "b",
        KeyC => "c",
        KeyD => "d",
        KeyE => "e",
        KeyF => "f",
        KeyG => "g",
        KeyH => "h",
        KeyI => "i",
        KeyJ => "j",
        KeyK => "k",
        KeyL => "l",
        KeyM => "m",
        KeyN => "n",
        KeyO => "o",
        KeyP => "p",
        KeyQ => "q",
        KeyR => "r",
        KeyS => "s",
        KeyT => "t",
        KeyU => "u",
        KeyV => "v",
        KeyW => "w",
        KeyX => "x",
        KeyY => "y",
        KeyZ => "z",
        Numpad0 => "0",
        Numpad1 => "1",
        Numpad2 => "2",
        Numpad3 => "3",
        Numpad4 => "4",
        Numpad5 => "5",
        Numpad6 => "6",
        Numpad7 => "7",
        Numpad8 => "8",
        Numpad9 => "9",
        NumpadAdd => "+",
        NumpadSubtract => "-",
        NumpadMultiply | NumpadStar => "*",
        NumpadDivide => "/",
        NumpadDecimal => ".",
        NumpadComma => ",",
        NumpadEqual => "=",
        other => {
            let (plain, shifted) = match other {
                Digit0 => ("0", ")"),
                Digit1 => ("1", "!"),
                Digit2 => ("2", "@"),
                Digit3 => ("3", "#"),
                Digit4 => ("4", "$"),
                Digit5 => ("5", "%"),
                Digit6 => ("6", "^"),
                Digit7 => ("7", "&"),
                Digit8 => ("8", "*"),
                Digit9 => ("9", "("),
                Backquote => ("`", "~"),
                Minus => ("-", "_"),
                Equal => ("=", "+"),
                BracketLeft => ("[", "{"),
                BracketRight => ("]", "}"),
                Backslash => ("\\", "|"),
                Semicolon => (";", ":"),
                Quote => ("'", "\""),
                Comma => (",", "<"),
                Period => (".", ">"),
                Slash => ("/", "?"),
                _ => return None,
            };
            if shift { shifted } else { plain }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_letter_positions_have_stable_lowercase_names() {
        use KeyCode::*;
        let keys = [
            KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN,
            KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
        ];
        for (code, letter) in keys.into_iter().zip(b'a'..=b'z') {
            for shift in [false, true] {
                assert_eq!(
                    physical_text(code, shift),
                    Some(char::from(letter).to_string().as_str())
                );
            }
        }
    }

    #[test]
    fn typing_modifiers_remain_text_and_command_modifiers_are_physical() {
        use ModifiersState as M;
        for apple in [false, true] {
            for modifiers in [M::empty(), M::SHIFT, M::ALT, M::ALT | M::SHIFT] {
                assert!(!should_translate(true, modifiers, apple));
                assert!(should_translate(false, modifiers, apple));
            }
        }
        assert!(should_translate(true, M::SUPER, true));
        assert!(should_translate(true, M::SUPER | M::SHIFT, true));
        assert!(!should_translate(true, M::CONTROL, true));
        assert!(should_translate(true, M::CONTROL, false));
        assert!(!should_translate(true, M::CONTROL | M::ALT, false));
    }

    #[test]
    fn shifted_punctuation_preserves_existing_zoom_and_rebinding_chords() {
        assert_eq!(physical_text(KeyCode::Equal, false), Some("="));
        assert_eq!(physical_text(KeyCode::Equal, true), Some("+"));
        assert_eq!(physical_text(KeyCode::Comma, false), Some(","));
        assert_eq!(physical_text(KeyCode::Digit2, true), Some("@"));
        for code in [KeyCode::ArrowLeft, KeyCode::Home, KeyCode::ShiftLeft] {
            assert_eq!(physical_text(code, false), None);
        }
    }
}
