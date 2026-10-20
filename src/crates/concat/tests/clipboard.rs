// SPDX-License-Identifier: AGPL-3.0-or-later

//! Exercise native shortcut routing and real text fields with a fake clipboard.

#![cfg(not(target_os = "android"))]

#[path = "../src/keyboard.rs"]
mod keyboard;

use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Clipboard, Key, Platform, PlatformError, WindowAdapter, WindowEvent};
use slint::winit_030::winit::event::ElementState;
use slint::winit_030::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use std::cell::RefCell;
use std::rc::Rc;

// Slint generates public bindings for the private fixture's components.
#[allow(missing_docs)]
mod fixture {
    slint::slint! {
        import { Field } from "../ui/primitives/modal.slint";

        export component ClipboardFieldWindow inherits Window {
            width: 300px;
            height: 40px;
            forward-focus: commands;
            out property <string> value;
            out property <string> plain-text <=> plain.text;
            out property <string> last-command;
            out property <bool> last-control;
            out property <bool> last-shift;
            out property <int> command-count;
            out property <int> open-count;
            commands := FocusScope {
                KeyBinding {
                    keys: @keys(Control + O);
                    activated => { root.open-count += 1; }
                }
                key-pressed(event) => {
                    root.last-command = event.text;
                    root.last-control = event.modifiers.control;
                    root.last-shift = event.modifiers.shift;
                    root.command-count += 1;
                    return accept;
                }
                HorizontalLayout {
                    field := Field {
                        value: root.value;
                        secret: true;
                        technical: true;
                        edited(text) => { root.value = text; }
                    }
                    plain := TextInput {}
                    readonly := TextInput { text: "Read only"; read-only: true; }
                }
            }
            public function paste-value() { field.paste-value(); }
            public function focus-text() { plain.focus(); }
            public function end-text() { plain.set-selection-offsets(1000000000, 1000000000); }
            public function focus-commands() { commands.focus(); }
            public function focus-readonly() { readonly.focus(); }
        }
    }
}

use fixture::ClipboardFieldWindow;

struct ClipboardPlatform {
    window: Rc<MinimalSoftwareWindow>,
    clipboard: Rc<RefCell<Option<String>>>,
}

impl Platform for ClipboardPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn clipboard_text(&self, _clipboard: Clipboard) -> Option<String> {
        self.clipboard.borrow().clone()
    }

    fn set_clipboard_text(&self, text: &str, _clipboard: Clipboard) {
        *self.clipboard.borrow_mut() = Some(text.into());
    }
}

fn native_key(
    ui: &ClipboardFieldWindow,
    keyboard: &mut keyboard::PhysicalShortcuts,
    code: KeyCode,
    logical: &str,
    state: ElementState,
    repeat: bool,
) {
    if !keyboard.key_event(
        ui.window(),
        PhysicalKey::Code(code),
        false,
        state,
        repeat,
        false,
    ) {
        let text = logical.into();
        ui.window().dispatch_event(match (state, repeat) {
            (ElementState::Released, _) => WindowEvent::KeyReleased { text },
            (ElementState::Pressed, true) => WindowEvent::KeyPressRepeated { text },
            _ => WindowEvent::KeyPressed { text },
        });
    }
}

fn press(
    ui: &ClipboardFieldWindow,
    keyboard: &mut keyboard::PhysicalShortcuts,
    code: KeyCode,
    logical: &str,
) {
    native_key(ui, keyboard, code, logical, ElementState::Pressed, false);
    native_key(ui, keyboard, code, logical, ElementState::Released, false);
}

fn modifiers(
    ui: &ClipboardFieldWindow,
    keyboard: &mut keyboard::PhysicalShortcuts,
    command: bool,
    shift: bool,
) {
    let primary = if cfg!(any(target_os = "macos", target_os = "ios")) {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    keyboard.modifiers_changed(
        if command {
            primary
        } else {
            ModifiersState::empty()
        } | if shift {
            ModifiersState::SHIFT
        } else {
            ModifiersState::empty()
        },
    );
    // The backend maps native Command to Slint Control on Apple platforms.
    for (key, down) in [(Key::Control, command), (Key::Shift, shift)] {
        let text = key.into();
        ui.window().dispatch_event(if down {
            WindowEvent::KeyPressed { text }
        } else {
            WindowEvent::KeyReleased { text }
        });
    }
}

#[test]
fn physical_shortcuts_work_across_layouts_and_text_keeps_its_language() {
    let clipboard = Rc::new(RefCell::new(Some("sk_test_first_dummy".into())));
    slint::platform::set_platform(Box::new(ClipboardPlatform {
        window: MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer),
        clipboard: clipboard.clone(),
    }))
    .unwrap();
    let ui = ClipboardFieldWindow::new().unwrap();
    ui.show().unwrap();
    let mut keyboard = keyboard::PhysicalShortcuts::default();

    ui.invoke_paste_value();
    assert_eq!(ui.get_value(), "sk_test_first_dummy");

    *clipboard.borrow_mut() = Some("sk_test_replacement_dummy".into());
    ui.invoke_paste_value();
    assert_eq!(ui.get_value(), "sk_test_replacement_dummy");

    modifiers(&ui, &mut keyboard, true, false);
    // Russian, Caps Lock, Latin, French AZERTY and Greek logical text.
    for (select, paste) in [
        ("ф", "м"),
        ("Ф", "М"),
        ("a", "v"),
        ("A", "V"),
        ("q", "v"),
        ("a", "ν"),
    ] {
        let value = format!("sk_test_{select}_dummy");
        *clipboard.borrow_mut() = Some(value.clone());
        press(&ui, &mut keyboard, KeyCode::KeyA, select);
        press(&ui, &mut keyboard, KeyCode::KeyV, paste);
        assert_eq!(ui.get_value(), value, "shortcut {select}/{paste}");
    }
    modifiers(&ui, &mut keyboard, false, false);

    *clipboard.borrow_mut() = None;
    let before = ui.get_value();
    ui.invoke_paste_value();
    assert_eq!(
        ui.get_value(),
        before,
        "missing clipboard leaves the key intact"
    );

    // The same native hook covers ordinary inputs, including copy and cut.
    ui.invoke_focus_text();
    for (code, text) in [
        (KeyCode::KeyG, "п"),
        (KeyCode::KeyH, "р"),
        (KeyCode::KeyB, "и"),
        (KeyCode::KeyD, "в"),
        (KeyCode::KeyT, "е"),
        (KeyCode::KeyN, "т"),
    ] {
        press(&ui, &mut keyboard, code, text);
    }
    assert_eq!(ui.get_plain_text(), "привет");
    modifiers(&ui, &mut keyboard, true, false);
    press(&ui, &mut keyboard, KeyCode::KeyA, "ф");
    press(&ui, &mut keyboard, KeyCode::KeyC, "с");
    assert_eq!(clipboard.borrow().as_deref(), Some("привет"));
    press(&ui, &mut keyboard, KeyCode::KeyX, "ч");
    assert_eq!(ui.get_plain_text(), "");
    press(&ui, &mut keyboard, KeyCode::KeyV, "м");
    assert_eq!(ui.get_plain_text(), "привет");
    press(&ui, &mut keyboard, KeyCode::KeyZ, "я");
    assert_eq!(ui.get_plain_text(), "");
    if cfg!(target_os = "windows") {
        press(&ui, &mut keyboard, KeyCode::KeyY, "н");
    } else {
        modifiers(&ui, &mut keyboard, true, true);
        press(&ui, &mut keyboard, KeyCode::KeyZ, "Я");
    }
    assert_eq!(ui.get_plain_text(), "привет");
    modifiers(&ui, &mut keyboard, false, false);

    // Composed characters and multi-character text must not turn into ASCII.
    ui.invoke_end_text();
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::Quote,
        "é",
        ElementState::Pressed,
        false,
    );
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::Quote,
        "é",
        ElementState::Released,
        false,
    );
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::KeyE,
        "日本語",
        ElementState::Pressed,
        false,
    );
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::KeyE,
        "日本語",
        ElementState::Released,
        false,
    );
    assert_eq!(ui.get_plain_text(), "приветé日本語");

    // Global commands and fixed KeyBindings see the same physical positions.
    ui.invoke_focus_readonly();
    press(&ui, &mut keyboard, KeyCode::KeyB, "и");
    assert_eq!(ui.get_last_command(), "b");
    ui.invoke_focus_commands();
    for logical in ["b", "и", "И", "β", "ب", "ิ"] {
        press(&ui, &mut keyboard, KeyCode::KeyB, logical);
        assert_eq!(ui.get_last_command(), "b");
        assert!(!ui.get_last_control());
    }
    modifiers(&ui, &mut keyboard, true, true);
    press(&ui, &mut keyboard, KeyCode::KeyZ, "Я");
    assert_eq!(ui.get_last_command(), "z");
    assert!(ui.get_last_control());
    assert!(ui.get_last_shift());
    modifiers(&ui, &mut keyboard, true, false);
    for logical in ["o", "щ", "Щ", "ο"] {
        press(&ui, &mut keyboard, KeyCode::KeyO, logical);
    }
    assert_eq!(ui.get_open_count(), 4);
    modifiers(&ui, &mut keyboard, false, false);

    let before = ui.get_command_count();
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::KeyH,
        "р",
        ElementState::Pressed,
        false,
    );
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::KeyH,
        "Р",
        ElementState::Pressed,
        true,
    );
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::KeyH,
        "h",
        ElementState::Released,
        false,
    );
    assert_eq!(ui.get_last_command(), "h");
    assert_eq!(ui.get_command_count(), before + 2);
    assert!(!keyboard.key_event(
        ui.window(),
        PhysicalKey::Code(KeyCode::KeyB),
        false,
        ElementState::Pressed,
        false,
        true
    ));
    assert_eq!(ui.get_command_count(), before + 2);
    // Num Lock navigation and keys without a physical code remain native.
    assert!(!keyboard.key_event(
        ui.window(),
        PhysicalKey::Code(KeyCode::Numpad7),
        true,
        ElementState::Pressed,
        false,
        false
    ));
    assert!(!keyboard.key_event(
        ui.window(),
        PhysicalKey::Unidentified(slint::winit_030::winit::keyboard::NativeKeyCode::Unidentified),
        false,
        ElementState::Pressed,
        false,
        false
    ));
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::NumpadDecimal,
        ",",
        ElementState::Pressed,
        false,
    );
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::NumpadDecimal,
        ",",
        ElementState::Released,
        false,
    );
    assert_eq!(ui.get_last_command(), ".");

    // Focus loss clears a held command before the next editing session.
    native_key(
        &ui,
        &mut keyboard,
        KeyCode::KeyB,
        "и",
        ElementState::Pressed,
        false,
    );
    keyboard.focus_lost();
    ui.invoke_focus_text();
    let before = ui.get_plain_text().to_string();
    press(&ui, &mut keyboard, KeyCode::KeyB, "и");
    assert_eq!(ui.get_plain_text(), before + "и");
}
