use crate::canvas::{Canvas, PaintCache, layer_color};
use eframe::egui::{self, Color32, Key, PointerButton, RichText};
use fschem::{
    geometry::{Point, Viewport},
    library::LibraryPaths,
    navigation::{self, Descent, DocumentTarget},
    scene::LoadedDocument,
    stroke_font::StrokeFont,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

const DEMO: &str = include_str!("../examples/demo.sch");

fn canvas_id() -> egui::Id {
    egui::Id::new("schematic_canvas")
}

pub struct Options {
    pub input: Option<PathBuf>,
    pub libraries: LibraryPaths,
    pub smoke_test: bool,
    pub screenshot: Option<PathBuf>,
}
struct DocumentView {
    loaded: Arc<LoadedDocument>,
    cache: PaintCache,
    viewport: Viewport,
    layers: BTreeMap<u16, bool>,
    selected: Option<usize>,
    fit: bool,
    target: Option<DocumentTarget>,
}
impl DocumentView {
    fn new(loaded: Arc<LoadedDocument>, cache: PaintCache) -> Self {
        let target = loaded.path.clone().map(DocumentTarget::File);
        Self {
            layers: loaded
                .scene
                .layers
                .keys()
                .map(|&layer| (layer, true))
                .collect(),
            loaded,
            cache,
            viewport: Viewport::default(),
            selected: None,
            fit: true,
            target,
        }
    }
    fn name(&self) -> String {
        self.target
            .as_ref()
            .map(DocumentTarget::name)
            .unwrap_or_else(|| "Demo.sch".into())
    }
}
struct Completion {
    id: u64,
    push_frame: bool,
    target: Option<DocumentTarget>,
    result: Result<(Arc<LoadedDocument>, PaintCache), String>,
}
enum Action {
    Open(PathBuf),
    OpenDialog,
    Demo,
    Reload,
    Back,
    Descend(Descent),
    Fit,
}

pub struct Viewer {
    #[cfg(target_os = "macos")]
    native_menu: Option<crate::macos_menu::NativeMenu>,
    current: Option<DocumentView>,
    /// One application-level stack shared by every symbol/schematic transition.
    navigation_stack: Vec<DocumentView>,
    paths: LibraryPaths,
    sender: mpsc::Sender<Completion>,
    receiver: mpsc::Receiver<Completion>,
    request: u64,
    loading: Option<PathBuf>,
    error: Option<String>,
    show_grid: bool,
    grid_spacing: f64,
    show_hidden: bool,
    show_libraries: bool,
    show_source: bool,
    show_diagnostics: bool,
    pointer: Option<Point>,
    smoke: bool,
    smoke_success: Arc<AtomicBool>,
    ready_frames: usize,
    screenshot: Option<PathBuf>,
    screenshot_requested: bool,
}
impl Viewer {
    pub fn new(ctx: &egui::Context, options: Options, smoke_success: Arc<AtomicBool>) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        let (sender, receiver) = mpsc::channel();
        let mut viewer = Self {
            #[cfg(target_os = "macos")]
            native_menu: None,
            current: None,
            navigation_stack: Vec::new(),
            paths: options.libraries,
            sender,
            receiver,
            request: 0,
            loading: None,
            error: None,
            show_grid: true,
            grid_spacing: 20.,
            show_hidden: false,
            show_libraries: false,
            show_source: false,
            show_diagnostics: false,
            pointer: None,
            smoke: options.smoke_test,
            smoke_success,
            ready_frames: 0,
            screenshot: options.screenshot,
            screenshot_requested: false,
        };
        if let Some(path) = options.input {
            viewer.load(ctx, Some(path), false);
        } else if viewer.smoke || viewer.screenshot.is_some() {
            viewer.load(ctx, None, false);
        }
        viewer
    }
    #[cfg(target_os = "macos")]
    pub fn install_native_menu(&mut self, ctx: &egui::Context) {
        self.native_menu = Some(crate::macos_menu::NativeMenu::new(ctx));
        self.sync_native_menu();
    }

    #[cfg(target_os = "macos")]
    fn sync_native_menu(&self) {
        if let Some(menu) = &self.native_menu {
            menu.sync(
                self.current.is_some(),
                self.loading.is_none()
                    && self
                        .current
                        .as_ref()
                        .is_some_and(|view| view.selected.is_some()),
                !self.navigation_stack.is_empty() || self.loading.is_some(),
                [
                    self.show_grid,
                    self.show_hidden,
                    self.show_source,
                    self.show_diagnostics,
                    self.show_libraries,
                ],
            );
        }
    }

    fn load(&mut self, ctx: &egui::Context, path: Option<PathBuf>, history: bool) {
        self.load_target(ctx, path.map(DocumentTarget::File), history);
    }
    fn load_target(
        &mut self,
        ctx: &egui::Context,
        target: Option<DocumentTarget>,
        push_frame: bool,
    ) {
        self.request += 1;
        self.loading = Some(PathBuf::from(
            target
                .as_ref()
                .map(DocumentTarget::name)
                .unwrap_or_else(|| "Built-in demo".into()),
        ));
        self.error = None;
        self.ready_frames = 0;
        let id = self.request;
        let paths = self.paths.clone();
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = if let Some(target) = &target {
                target.load(paths)
            } else {
                fschem::document::Document::parse(DEMO)
                    .map_err(|e| e.to_string())
                    .and_then(|doc| LoadedDocument::from_document(Arc::new(doc), None, paths))
            }
            .map(|loaded| {
                let cache = PaintCache::new(&loaded.scene);
                (Arc::new(loaded), cache)
            });
            let _ = sender.send(Completion {
                id,
                push_frame,
                target,
                result,
            });
            ctx.request_repaint();
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(completion) = self.receiver.try_recv() {
            if completion.id != self.request {
                continue;
            }
            self.loading = None;
            match completion.result {
                Ok((loaded, cache)) => {
                    let mut view = DocumentView::new(loaded, cache);
                    view.target = completion.target;
                    let name = view.name();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — fschem")));
                    if let Some(old) = self.current.take().filter(|_| completion.push_frame) {
                        self.navigation_stack.push(old);
                    }
                    self.current = Some(view);
                }
                Err(error) => {
                    eprintln!("{error}");
                    self.error = Some(error);
                }
            }
        }
    }
    fn action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Open(path) => self.load(ctx, Some(path), true),
            Action::OpenDialog => {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("Xschem schematic or symbol", &["sch", "sym"]);
                if let Some(dir) = self
                    .current
                    .as_ref()
                    .and_then(|v| v.target.as_ref())
                    .map(DocumentTarget::directory)
                {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    self.load(ctx, Some(path), true);
                }
            }
            Action::Demo => self.load(ctx, None, true),
            Action::Reload => {
                if let Some(current) = &self.current {
                    self.load_target(ctx, current.target.clone(), false);
                }
            }
            Action::Back => {
                // Cancel any in-flight descent, even if still at the root. A
                // stale completion must never reopen a child after Shift-E.
                self.request += 1;
                self.loading = None;
                self.error = None;
                if let Some(previous) = self.navigation_stack.pop() {
                    let name = previous.name();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — fschem")));
                    self.current = Some(previous);
                }
            }
            Action::Descend(kind) => {
                if self.loading.is_some() {
                    return;
                }
                let Some(current) = &self.current else {
                    return;
                };
                let Some(instance) = current
                    .selected
                    .and_then(|i| current.loaded.scene.instances.get(i))
                else {
                    self.error = Some("Select an instance before descending.".into());
                    return;
                };
                match navigation::descend(instance, kind, &self.paths) {
                    Ok(target) => self.load_target(ctx, Some(target), true),
                    Err(error) => self.error = Some(error),
                }
            }
            Action::Fit => {
                if let Some(current) = &mut self.current {
                    current.fit = true;
                }
            }
        }
        ctx.request_repaint();
    }
    fn show(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        let mut actions = Vec::new();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.native_menu {
            use crate::macos_menu::Command;
            for command in menu.commands() {
                match command {
                    Command::Open => actions.push(Action::OpenDialog),
                    Command::Demo => actions.push(Action::Demo),
                    Command::Reload => actions.push(Action::Reload),
                    Command::Fit => actions.push(Action::Fit),
                    Command::Grid => self.show_grid = !self.show_grid,
                    Command::Hidden => self.show_hidden = !self.show_hidden,
                    Command::Source => self.show_source = !self.show_source,
                    Command::Diagnostics => self.show_diagnostics = !self.show_diagnostics,
                    Command::Libraries => self.show_libraries = true,
                    Command::Symbol => actions.push(Action::Descend(Descent::Symbol)),
                    Command::Schematic => actions.push(Action::Descend(Descent::Schematic)),
                    Command::Back => actions.push(Action::Back),
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        ctx.input_mut(|input| {
            if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                Key::O,
            )) {
                actions.push(Action::OpenDialog);
            }
            if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                Key::R,
            )) {
                actions.push(Action::Reload);
            }
        });
        if !ctx.wants_keyboard_input() || ctx.memory(|memory| memory.has_focus(canvas_id())) {
            ctx.input(|input| {
                if !input.modifiers.any()
                    && (input.key_pressed(Key::F) || input.key_pressed(Key::Home))
                {
                    actions.push(Action::Fit);
                }
                if input.modifiers == egui::Modifiers::NONE && input.key_pressed(Key::I) {
                    actions.push(Action::Descend(Descent::Symbol));
                }
                if input.modifiers == egui::Modifiers::NONE && input.key_pressed(Key::E) {
                    actions.push(Action::Descend(Descent::Schematic));
                }
                if (input.modifiers == egui::Modifiers::SHIFT && input.key_pressed(Key::E))
                    || (input.modifiers == egui::Modifiers::NONE
                        && input.key_pressed(Key::Backspace))
                {
                    actions.push(Action::Back);
                }
            });
        }
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        });
        if let Some(path) = dropped {
            actions.push(Action::Open(path));
        }

        #[cfg(not(target_os = "macos"))]
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.label(
                    RichText::new("fschem")
                        .strong()
                        .color(Color32::from_rgb(120, 220, 160)),
                );
                ui.menu_button("File", |ui| {
                    if ui.button("Open…     Ctrl/Command-O").clicked() {
                        actions.push(Action::OpenDialog);
                        ui.close();
                    }
                    if ui.button("Built-in demonstration").clicked() {
                        actions.push(Action::Demo);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.current.is_some(),
                            egui::Button::new("Reload     Ctrl/Command-R"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Reload);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Fit drawing     F / Home").clicked() {
                        actions.push(Action::Fit);
                        ui.close();
                    }
                    ui.checkbox(&mut self.show_grid, "Grid");
                    ui.checkbox(&mut self.show_hidden, "Hidden text");
                    ui.checkbox(&mut self.show_source, "Original file text");
                    ui.checkbox(&mut self.show_diagnostics, "Diagnostics");
                });
                ui.menu_button("Navigate", |ui| {
                    let selected = self.loading.is_none()
                        && self
                            .current
                            .as_ref()
                            .is_some_and(|view| view.selected.is_some());
                    if ui
                        .add_enabled(selected, egui::Button::new("Descend symbol     i"))
                        .clicked()
                    {
                        actions.push(Action::Descend(Descent::Symbol));
                        ui.close();
                    }
                    if ui
                        .add_enabled(selected, egui::Button::new("Descend schematic     e"))
                        .clicked()
                    {
                        actions.push(Action::Descend(Descent::Schematic));
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.navigation_stack.is_empty() || self.loading.is_some(),
                            egui::Button::new("Up one level     Shift-E"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Back);
                        ui.close();
                    }
                });
                if ui.button("Libraries…").clicked() {
                    self.show_libraries = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("READ-ONLY · Xschem files");
                });
            });
        });
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open…").clicked() {
                    actions.push(Action::OpenDialog);
                }
                if ui
                    .add_enabled(
                        !self.navigation_stack.is_empty() || self.loading.is_some(),
                        egui::Button::new("↑ Up (Shift-E)"),
                    )
                    .clicked()
                {
                    actions.push(Action::Back);
                }
                if ui
                    .add_enabled(self.current.is_some(), egui::Button::new("Fit"))
                    .clicked()
                {
                    actions.push(Action::Fit);
                }
                ui.separator();
                ui.weak(format!("Depth {}", self.navigation_stack.len()));
                ui.checkbox(&mut self.show_grid, "Grid");
                ui.add(
                    egui::DragValue::new(&mut self.grid_spacing)
                        .range(0.001..=1e6)
                        .speed(1.)
                        .prefix("Spacing "),
                );
                if let Some(current) = &self.current {
                    ui.monospace(format!("{:.0}%", current.viewport.zoom * 100.));
                }
                if let Some(path) = &self.loading {
                    ui.spinner();
                    ui.label(format!(
                        "Loading {}…",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(current) = &self.current {
                    ui.label(format!(
                        "{} records · {} instances · {} drawing items",
                        current.loaded.document.records.len(),
                        current.loaded.scene.instances.len(),
                        current.loaded.scene.items.len()
                    ));
                    if !current.loaded.scene.diagnostics.is_empty()
                        && ui
                            .button(
                                RichText::new(format!(
                                    "{} diagnostics",
                                    current.loaded.scene.diagnostics.len()
                                ))
                                .color(Color32::YELLOW),
                            )
                            .clicked()
                    {
                        self.show_diagnostics = true;
                    }
                } else {
                    ui.label("Open a .sch or .sym file, or drag one into this window");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(p) = self.pointer {
                        ui.monospace(format!("x {:8.2}   y {:8.2}", p.x, p.y));
                    }
                });
            });
        });
        egui::SidePanel::right("inspector").default_width(280.).min_width(200.).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Document");
                if let Some(current) = &mut self.current {
                    let loaded = &current.loaded;
                    ui.label(current.target.as_ref().map(DocumentTarget::description).unwrap_or_else(|| "Built-in demo (embedded symbols)".into()));
                    ui.weak(format!("File format {} · {} bytes", loaded.document.version(), loaded.document.original().len()));
                    ui.weak("Bundled Xschem stroke font");
                    ui.separator();
                    egui::CollapsingHeader::new("Layers").default_open(true).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("All").clicked() { current.layers.values_mut().for_each(|v| *v = true); }
                            if ui.small_button("None").clicked() { current.layers.values_mut().for_each(|v| *v = false); }
                        });
                        for (&layer, visible) in &mut current.layers {
                            ui.horizontal(|ui| {
                                    ui.colored_label(layer_color(layer), "■");
                                ui.checkbox(visible, format!("{layer}: {} ({})", layer_name(layer), loaded.scene.layers[&layer]));
                            });
                        }
                    });
                    ui.separator();
                    ui.heading("Instance");
                    if let Some(instance) = current.selected.and_then(|i| loaded.scene.instances.get(i)) {
                        ui.strong(instance.properties.get("name").unwrap_or("Unnamed instance"));
                        ui.label(&instance.symbol);
                        ui.monospace(format!("({}, {}) · rot {} · flip {}", instance.at.x, instance.at.y, instance.rotation, instance.flip));
                        ui.horizontal(|ui| {
                            if ui.add_enabled(self.loading.is_none(), egui::Button::new("Symbol (i)")).clicked() { actions.push(Action::Descend(Descent::Symbol)); }
                            if ui.add_enabled(self.loading.is_none(), egui::Button::new("Schematic (e)")).clicked() { actions.push(Action::Descend(Descent::Schematic)); }
                        });
                        if instance.embedded { ui.weak("Definition embedded in this document"); }
                        else if instance.resolved.is_none() { ui.colored_label(Color32::LIGHT_RED, "Unresolved symbol — see diagnostics"); }
                        let preview = preview(&instance.properties.raw, 20_000);
                        let mut text = preview.as_str();
                        ui.add(egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).desired_rows(12));
                    } else { ui.weak("Left-click an instance to select it. i: symbol · e: schematic · Shift-E: up."); }
                } else { ui.weak("No document open"); }
            });
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                if let Some(current) = &mut self.current {
                    let (_, rect) = ui.allocate_space(ui.available_size());
                    let response = ui.interact(rect, canvas_id(), egui::Sense::click_and_drag());
                    let size = Point::new(rect.width() as f64, rect.height() as f64);
                    if current.fit && rect.width() > 0. && rect.height() > 0. {
                        let bounds = current
                            .loaded
                            .scene
                            .items
                            .iter()
                            .filter(|item| {
                                (!item.hidden || self.show_hidden)
                                    && current.layers.get(&item.layer).copied().unwrap_or(true)
                            })
                            .map(|item| item.bounds)
                            .reduce(|mut a, b| {
                                a.include(b);
                                a
                            })
                            .unwrap_or(current.loaded.scene.bounds);
                        current.viewport.fit(bounds, size);
                        current.fit = false;
                    }
                    if response.dragged_by(PointerButton::Middle) || response.dragged_by(PointerButton::Secondary) {
                        let delta = response.drag_delta();
                        current
                            .viewport
                            .pan(Point::new(delta.x as f64, delta.y as f64));
                    }
                    if response.hovered() {
                        let (scroll, zoom, shift, pointer) = ui.input(|input| {
                            (
                                input.raw_scroll_delta,
                                input.zoom_delta(),
                                input.modifiers.shift,
                                input.pointer.hover_pos(),
                            )
                        });
                        let p = pointer.unwrap_or(rect.center());
                        let anchor =
                            Point::new((p.x - rect.left()) as f64, (p.y - rect.top()) as f64);
                        if zoom != 1. {
                            current.viewport.zoom_at(zoom as f64, anchor, size);
                        } else if shift {
                            current
                                .viewport
                                .pan(Point::new(scroll.x as f64, scroll.y as f64));
                        } else if scroll.y != 0. {
                            current
                                .viewport
                                .zoom_at((scroll.y as f64 * 0.008).exp(), anchor, size);
                        }
                    }
                    let painter = ui.painter_at(rect);
                    let canvas = Canvas {
                        painter: &painter,
                        rect,
                        viewport: current.viewport,
                        font: StrokeFont::bundled(),
                    };
                    if self.show_grid {
                        canvas.grid(self.grid_spacing);
                    }
                    canvas.scene(
                        &current.loaded.scene,
                        &current.cache,
                        &current.layers,
                        self.show_hidden,
                        current.selected,
                    );
                    self.pointer = response.hover_pos().map(|pos| canvas.world(pos));
                    if response.clicked_by(PointerButton::Primary) {
                        response.request_focus();
                        let hit = response
                            .interact_pointer_pos()
                            .map(|p| canvas.world(p))
                            .and_then(|point| {
                                current
                                    .loaded
                                    .scene
                                    .instances
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, instance)| {
                                        instance
                                            .bounds
                                            .expanded(3. / current.viewport.zoom)
                                            .contains(point)
                                    })
                                    .min_by(|(_, a), (_, b)| {
                                        (a.bounds.size().x * a.bounds.size().y)
                                            .total_cmp(&(b.bounds.size().x * b.bounds.size().y))
                                    })
                                    .map(|(i, _)| i)
                            });
                        current.selected = hit;
                        if response.double_clicked_by(PointerButton::Primary) && hit.is_some() {
                            actions.push(Action::Descend(Descent::Symbol));
                        }
                        ctx.request_repaint();
                    }
                    response.on_hover_cursor(egui::CursorIcon::Default);
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(100.);
                        ui.heading(
                            RichText::new("fschem")
                                .size(48.)
                                .color(Color32::from_rgb(120, 220, 160)),
                        );
                        ui.label("A Rust + egui schematic editor foundation");
                        ui.add_space(18.);
                        ui.label(
                            "Open existing Xschem schematics and symbols without modifying them.",
                        );
                        ui.add_space(18.);
                        if ui.button("Open schematic or symbol…").clicked() {
                            actions.push(Action::OpenDialog);
                        }
                        if ui.button("Explore the self-contained demo").clicked() {
                            actions.push(Action::Demo);
                        }
                        ui.add_space(24.);
                        ui.weak(
                            "Left-click to select · Middle/right-drag to pan · Wheel to zoom · F to fit",
                        );
                    });
                }
            });
        self.windows(ctx, &mut actions);
        for action in actions {
            self.action(ctx, action);
        }
        #[cfg(target_os = "macos")]
        self.sync_native_menu();
        self.automation(ctx);
    }
    fn windows(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::Window::new("Symbol libraries").open(&mut self.show_libraries).default_width(620.).show(ctx, |ui| {
            ui.label("Resolution order: document directory, then these folders. Bare names also check each folder's devices/ subdirectory.");
            let mut remove = None;
            egui::ScrollArea::vertical().max_height(260.).show(ui, |ui| {
                for (i, path) in self.paths.roots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("×").clicked() { remove = Some(i); }
                        ui.label(path.display().to_string());
                        if !path.is_dir() { ui.weak("(not found)"); }
                    });
                }
            });
            if let Some(index) = remove { self.paths.roots.remove(index); }
            if ui.button("Add library folder…").clicked()
                && let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.paths.roots.insert(0, folder);
            }
            if ui.button("Reload current document with these paths").clicked() { actions.push(Action::Reload); }
        });
        egui::Window::new("Original file text · read-only")
            .open(&mut self.show_source)
            .default_size([800., 550.])
            .show(ctx, |ui| {
                if let Some(current) = &self.current {
                    let text = preview(current.loaded.document.original(), 100_000);
                    let mut text = text.as_str();
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .code_editor()
                                .desired_width(f32::INFINITY),
                        );
                    });
                }
            });
        egui::Window::new("Compatibility diagnostics")
            .open(&mut self.show_diagnostics)
            .default_size([850., 400.])
            .show(ctx, |ui| {
                if let Some(current) = &self.current {
                    let diagnostics = &current.loaded.scene.diagnostics;
                    if diagnostics.is_empty() {
                        ui.label("No loader diagnostics.");
                    }
                    egui::ScrollArea::both().show_rows(ui, 22., diagnostics.len(), |ui, range| {
                        for diagnostic in &diagnostics[range] {
                            ui.horizontal(|ui| {
                                ui.monospace(format!(
                                    "{}:{}",
                                    diagnostic
                                        .file
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy(),
                                    diagnostic.line
                                ))
                                .on_hover_text(diagnostic.file.display().to_string());
                                ui.add(egui::Label::new(&diagnostic.message).truncate())
                                    .on_hover_text(&diagnostic.message);
                            });
                        }
                    });
                }
            });
        if let Some(error) = self.error.clone() {
            egui::Window::new("Could not open document")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.colored_label(Color32::LIGHT_RED, error);
                    ui.label(if self.current.is_some() {
                        "The previous document is still open."
                    } else {
                        "Choose a readable Xschem schematic or symbol file."
                    });
                    if ui.button("Close").clicked() {
                        self.error = None;
                    }
                });
        }
    }
    fn automation(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        if self.smoke && self.error.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.current.is_none() {
            return;
        }
        if self.smoke || self.screenshot.is_some() {
            self.ready_frames += 1;
            ctx.request_repaint();
            if self.ready_frames >= 3 {
                if self.screenshot.is_some() && !self.screenshot_requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                    self.screenshot_requested = true;
                } else if self.screenshot.is_none() && self.smoke {
                    self.smoke_success.store(true, Ordering::Relaxed);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });
        if let (Some(image), Some(path)) = (screenshot, self.screenshot.as_ref()) {
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            match image::save_buffer(
                path,
                &bytes,
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            ) {
                Ok(()) => {
                    eprintln!("Screenshot saved to {}", path.display());
                    self.screenshot = None;
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.screenshot = None;
                }
            }
        }
    }
}
impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.show(ctx);
    }
}

fn layer_name(layer: u16) -> &'static str {
    match layer {
        0 => "background",
        1 => "wires",
        2 => "grid / graphs",
        3 => "text",
        4 => "symbols",
        5 => "pins",
        _ => "drawing",
    }
}
fn preview(text: &str, count: usize) -> String {
    if let Some((index, _)) = text.char_indices().nth(count) {
        format!(
            "{}\n\n[Preview truncated; full original text is retained.]",
            &text[..index]
        )
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_stack_smoke() {
        use std::{
            fs,
            time::{Duration, Instant},
        };

        fn settle(viewer: &mut Viewer, ctx: &egui::Context) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while viewer.loading.is_some() {
                viewer.poll(ctx);
                assert!(Instant::now() < deadline, "navigation timed out");
                std::thread::yield_now();
            }
            assert!(viewer.error.is_none(), "{:?}", viewer.error);
        }
        fn key(viewer: &mut Viewer, ctx: &egui::Context, key: Key, modifiers: egui::Modifiers) {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 800.),
                    )),
                    modifiers,
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                },
                |ctx| viewer.show(ctx),
            );
            settle(viewer, ctx);
        }

        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("navigation-smoke-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("parent.sch"),
            "v {xschem file_version=1.3}\nC {cell.sym} 0 0 0 0 {name=X1}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cell.sym"),
            "v {xschem file_version=1.3}\nK {type=subcircuit}\nB 4 -10 -10 10 10 {fill=false}\n",
        )
        .unwrap();
        fs::write(
            dir.join("cell.sch"),
            "v {xschem file_version=1.3}\nC {cell.sym} 0 0 0 0 {name=X2}\n",
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(
            &ctx,
            Options {
                input: Some(dir.join("parent.sch")),
                libraries: LibraryPaths::default(),
                smoke_test: false,
                screenshot: None,
            },
            Arc::new(AtomicBool::new(false)),
        );
        settle(&mut viewer, &ctx);
        let parent = viewer.current.as_mut().unwrap();
        parent.selected = Some(0);
        parent.viewport = Viewport {
            center: Point::new(19., -37.),
            zoom: 2.5,
        };
        parent.fit = false;

        key(&mut viewer, &ctx, Key::E, egui::Modifiers::NONE);
        assert_eq!(viewer.current.as_ref().unwrap().name(), "cell.sch");
        assert_eq!(viewer.navigation_stack.len(), 1);
        let child = viewer.current.as_mut().unwrap();
        child.selected = Some(0);
        child.layers.insert(4, false);
        key(&mut viewer, &ctx, Key::I, egui::Modifiers::NONE);
        assert_eq!(viewer.current.as_ref().unwrap().name(), "cell.sym");
        assert_eq!(viewer.navigation_stack.len(), 2);

        key(&mut viewer, &ctx, Key::E, egui::Modifiers::SHIFT);
        let child = viewer.current.as_ref().unwrap();
        assert_eq!(child.name(), "cell.sch");
        assert_eq!(child.selected, Some(0));
        assert!(!child.layers[&4]);
        assert_eq!(viewer.navigation_stack.len(), 1);
        key(&mut viewer, &ctx, Key::E, egui::Modifiers::SHIFT);
        let parent = viewer.current.as_ref().unwrap();
        assert_eq!(parent.name(), "parent.sch");
        assert_eq!(parent.selected, Some(0));
        assert_eq!(parent.viewport.center, Point::new(19., -37.));
        assert_eq!(parent.viewport.zoom, 2.5);
        assert!(viewer.navigation_stack.is_empty());
        key(&mut viewer, &ctx, Key::E, egui::Modifiers::SHIFT);
        assert_eq!(viewer.current.as_ref().unwrap().name(), "parent.sch");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn egui_frame_paints_self_contained_demo_without_an_os_window() {
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(
            &ctx,
            Options {
                input: None,
                libraries: LibraryPaths::default(),
                smoke_test: false,
                screenshot: None,
            },
            Arc::new(AtomicBool::new(false)),
        );
        let loaded = Arc::new(
            LoadedDocument::from_document(
                Arc::new(fschem::document::Document::parse(DEMO).unwrap()),
                None,
                LibraryPaths::default(),
            )
            .unwrap(),
        );
        assert!(loaded.scene.diagnostics.is_empty());
        let cache = PaintCache::new(&loaded.scene);
        viewer.current = Some(DocumentView::new(loaded, cache));
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200., 800.),
                )),
                ..Default::default()
            },
            |ctx| viewer.show(ctx),
        );
        assert!(output.shapes.len() > 100);
        assert!(
            !ctx.tessellate(output.shapes, output.pixels_per_point)
                .is_empty()
        );
    }
}
