//! Main-thread AppKit menus. Callbacks queue commands instead of borrowing the viewer.
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{NSApp, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSObject, NSString};

#[derive(Clone, Copy)]
pub enum Command {
    Open,
    Demo,
    Reload,
    Fit,
    Grid,
    Hidden,
    Source,
    Diagnostics,
    Libraries,
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
        let mut items = Vec::new();
        for (title, entries) in [
            (
                "File",
                vec![
                    ("Open…", "o", 0),
                    ("Built-in demonstration", "", 1),
                    ("Reload", "r", 2),
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
                item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
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
                    "View" => 2,
                    _ => 3,
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

    pub fn sync(&self, document: bool, selected: bool, back: bool, checked: [bool; 5]) {
        self.items[2].setEnabled(document);
        self.items[3].setEnabled(document);
        self.items[9].setEnabled(selected);
        self.items[10].setEnabled(selected);
        self.items[11].setEnabled(back);
        for (item, checked) in self.items[4..9].iter().zip(checked) {
            item.setState(if checked { 1 } else { 0 });
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
