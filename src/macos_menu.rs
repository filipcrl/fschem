//! Main-thread AppKit menus. Callbacks queue commands instead of borrowing the viewer.
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{NSApp, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSObject, NSString};

#[derive(Clone, Copy)]
pub enum Command {
    Undo,
    Redo,
    Edit(egui::Key),
    Open,
    OpenText,
    Save,
    SaveAs,
    CloseTab,
    Quit,
    Demo,
    Reload,
    ExportSpice,
    Simulate,
    Simulator(fschem::simulation::Simulator),
    Fit,
    Grid,
    Hidden,
    Source,
    Diagnostics,
    Libraries,
    Fps,
    Symbol,
    Schematic,
    Back,
}

pub struct HandlerState {
    sender: Sender<Command>,
    ctx: egui::Context,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = HandlerState]
    #[name = "FschemMenuHandler"]
    struct MenuHandler;

    impl MenuHandler {
        #[unsafe(method(performMenuCommand:))]
        fn perform_command(&self, sender: &NSMenuItem) {
            let command = match sender.tag() {
                0 => Command::Open,
                40 => Command::OpenText,
                1 => Command::Demo,
                2 => Command::Reload,
                3 => Command::Fit,
                4 => Command::Grid,
                5 => Command::Hidden,
                6 => Command::Source,
                7 => Command::Diagnostics,
                8 => Command::Libraries,
                9 => Command::Symbol,
                10 => Command::Schematic,
                11 => Command::Back,
                12 => Command::ExportSpice,
                13 => Command::Save,
                14 => Command::SaveAs,
                15 => Command::Fps,
                16 => Command::CloseTab,
                17 => Command::Quit,
                18 => Command::Simulator(fschem::simulation::Simulator::Ngspice),
                19 => Command::Simulator(fschem::simulation::Simulator::Vacask),
                20 => Command::Simulate,
                100 => Command::Edit(egui::Key::G),
                101 => Command::Edit(egui::Key::C),
                102 => Command::Edit(egui::Key::Backspace),
                103 => Command::Edit(egui::Key::W),
                104 => Command::Edit(egui::Key::R),
                105 => Command::Edit(egui::Key::X),
                106 => Command::Edit(egui::Key::Y),
                107 => Command::Edit(egui::Key::Escape),
                108 => Command::Undo,
                109 => Command::Redo,
                _ => return,
            };
            let _ = self.ivars().sender.send(command);
            self.ivars().ctx.request_repaint();
        }
    }
);

pub struct NativeMenu {
    // NSMenuItem targets are not retained by AppKit.
    _handler: Retained<MenuHandler>,
    receiver: Receiver<Command>,
    items: Vec<Retained<NSMenuItem>>,
}

impl NativeMenu {
    pub fn new(ctx: &egui::Context) -> Self {
        let mtm = MainThreadMarker::new().expect("native menus require the main thread");
        let (sender, receiver) = mpsc::channel();
        let handler = MenuHandler::alloc(mtm).set_ivars(HandlerState {
            sender,
            ctx: ctx.clone(),
        });
        // SAFETY: initialize our allocated NSObject subclass on the main thread.
        let handler: Retained<MenuHandler> = unsafe { msg_send![super(handler), init] };
        let app = NSApp(mtm);
        let main = app
            .mainMenu()
            .expect("eframe must initialize the application menu first");
        // Route the application menu's Quit through eframe's cancellable close
        // request instead of AppKit's immediate terminate: action.
        if let Some(app_menu) = main.itemAtIndex(0).and_then(|item| item.submenu()) {
            for item in app_menu.itemArray() {
                if item.action() == Some(sel!(terminate:)) {
                    item.setTag(17);
                    unsafe {
                        item.setAction(Some(sel!(performMenuCommand:)));
                        item.setTarget(Some(&handler));
                    }
                }
            }
        }
        let mut items = Vec::new();
        for (title, entries) in [
            (
                "File",
                vec![
                    ("Open…", "o", 0),
                    ("Open as text…", "", 40),
                    ("Save", "s", 13),
                    ("Save As…", "s", 14),
                    ("Close tab", "w", 16),
                    ("Built-in demonstration", "", 1),
                    ("Reload", "r", 2),
                    ("Export SPICE netlist…", "", 12),
                ],
            ),
            (
                "Edit",
                vec![
                    ("Undo", "z", 108),
                    ("Redo", "z", 109),
                    ("Move (G)", "", 100),
                    ("Copy (C)", "", 101),
                    ("Delete (Backspace)", "", 102),
                    ("Add wire (W)", "", 103),
                    ("Rotate 90° (R)", "", 104),
                    ("Mirror across X axis (X)", "", 105),
                    ("Mirror across Y axis (Y)", "", 106),
                    ("Cancel (Esc)", "", 107),
                ],
            ),
            (
                "View",
                vec![
                    ("Fit drawing (F / Home)", "", 3),
                    ("Grid", "", 4),
                    ("Hidden text", "", 5),
                    ("Original file text", "", 6),
                    ("Diagnostics", "", 7),
                    ("Libraries…", ",", 8),
                    ("FPS counter", "", 15),
                ],
            ),
            (
                "Navigate",
                vec![
                    ("Descend symbol (I)", "", 9),
                    ("Descend schematic (E)", "", 10),
                    ("Up one level (Shift-E)", "", 11),
                ],
            ),
            (
                "Simulator",
                vec![("ngspice", "", 18), ("VACASK", "", 19), ("Run…", "", 20)],
            ),
        ] {
            let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(title));
            menu.setAutoenablesItems(false);
            for (title, key, tag) in entries {
                // SAFETY: selector has the expected action signature; target lives with Self.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        mtm.alloc(),
                        &NSString::from_str(title),
                        Some(sel!(performMenuCommand:)),
                        &NSString::from_str(key),
                    )
                };
                item.setTag(tag);
                item.setKeyEquivalentModifierMask(if tag == 14 || tag == 109 {
                    NSEventModifierFlags::Command | NSEventModifierFlags::Shift
                } else {
                    NSEventModifierFlags::Command
                });
                unsafe {
                    item.setTarget(Some(&handler));
                }
                menu.addItem(&item);
                items.push(item);
            }
            let root = NSMenuItem::new(mtm);
            root.setSubmenu(Some(&menu));
            // Keep the application menu first and system Window/Help menus last.
            main.insertItem_atIndex(
                &root,
                match title {
                    "File" => 1,
                    "Edit" => 2,
                    "View" => 3,
                    "Navigate" => 4,
                    _ => 5,
                },
            );
        }
        Self {
            _handler: handler,
            receiver,
            items,
        }
    }

    pub fn commands(&self) -> Vec<Command> {
        self.receiver.try_iter().collect()
    }
    pub fn sync_simulator(&self, simulator: fschem::simulation::Simulator) {
        for item in &self.items {
            let selected = match item.tag() {
                18 => simulator == fschem::simulation::Simulator::Ngspice,
                19 => simulator == fschem::simulation::Simulator::Vacask,
                _ => continue,
            };
            item.setState(if selected { 1 } else { 0 });
        }
    }

    pub fn sync_history(&self, undo: Option<&str>, redo: Option<&str>) {
        for item in &self.items {
            let (action, label) = match item.tag() {
                108 => ("Undo", undo),
                109 => ("Redo", redo),
                _ => continue,
            };
            item.setEnabled(label.is_some());
            let title =
                label.map_or_else(|| action.to_owned(), |label| format!("{action} {label}"));
            item.setTitle(&NSString::from_str(&title));
        }
    }

    pub fn sync(
        &self,
        document: bool,
        export: bool,
        selected: bool,
        back: bool,
        checked: [bool; 6],
        edit: [bool; 4],
    ) {
        for item in &self.items {
            match item.tag() {
                2 | 3 => item.setEnabled(document),
                12 | 20 => item.setEnabled(export),
                13 | 14 => item.setEnabled(edit[1]),
                100..=102 => item.setEnabled(edit[0]),
                103 => item.setEnabled(edit[1]),
                104..=106 => item.setEnabled(edit[2]),
                107 => item.setEnabled(edit[3]),
                9 | 10 => item.setEnabled(selected),
                11 => item.setEnabled(back),
                _ => {}
            }
            let checked_index = match item.tag() {
                4..=8 => Some(item.tag() as usize - 4),
                15 => Some(5),
                _ => None,
            };
            if let Some(index) = checked_index {
                item.setState(if checked[index] { 1 } else { 0 });
            }
        }
    }
}

impl Drop for NativeMenu {
    fn drop(&mut self) {
        // AppKit may keep the main menu alive beyond the viewer during shutdown.
        // Clear its unretained targets before releasing the handler.
        for item in &self.items {
            unsafe { item.setTarget(None) };
            item.setEnabled(false);
        }
    }
}
