use crate::canvas::{Canvas, PaintCache, layer_color};
use eframe::egui::{self, Color32, Key, PointerButton, RichText};
use fschem::{
    document::{Document, RecordKind},
    edit::{CommandHistory, DocumentCommand, EditOperation, EditState, Placement},
    geometry::{Bounds, Point, Viewport},
    library::LibraryPaths,
    navigation::{self, Descent, DocumentTarget},
    scene::{LoadedDocument, Selection},
    stroke_font::StrokeFont,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
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
    saved: Arc<Document>,
    cache: PaintCache,
    viewport: Viewport,
    layers: BTreeMap<u16, bool>,
    selected: BTreeSet<Selection>,
    selection_box: Option<Point>,
    fit: bool,
    target: Option<DocumentTarget>,
    moving: Option<MoveState>,
    wiring: Option<WireState>,
    history: CommandHistory,
}
struct MoveState {
    original: Arc<LoadedDocument>,
    restore: Arc<LoadedDocument>,
    selection_before: BTreeSet<Selection>,
    copying: bool,
    records: BTreeSet<usize>,
    pivot: Point,
    grab: Point,
    source: PathBuf,
    at: Point,
    rotation: u8,
    flip: bool,
}
struct WireState {
    start: Point,
    end: Point,
    on_connection: bool,
}
impl DocumentView {
    fn new(loaded: Arc<LoadedDocument>, cache: PaintCache) -> Self {
        let target = loaded.path.clone().map(DocumentTarget::File);
        Self {
            saved: Arc::clone(&loaded.document),
            layers: loaded
                .scene
                .layers
                .keys()
                .map(|&layer| (layer, true))
                .collect(),
            loaded,
            cache,
            viewport: Viewport::default(),
            selected: BTreeSet::new(),
            selection_box: None,
            fit: true,
            target,
            moving: None,
            wiring: None,
            history: CommandHistory::default(),
        }
    }
    fn name(&self) -> String {
        self.target
            .as_ref()
            .map(DocumentTarget::name)
            .unwrap_or_else(|| "Demo.sch".into())
    }
    fn modified(&self) -> bool {
        self.loaded.document.original() != self.saved.original()
    }
    fn save_to(&mut self, path: &Path, paths: &LibraryPaths) -> Result<(), String> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let loaded = LoadedDocument::from_document(
            Arc::clone(&self.loaded.document),
            Some(path.clone()),
            paths.clone(),
        )?;
        loaded.document.save(&path)?;
        self.saved = Arc::clone(&loaded.document);
        self.cache = PaintCache::new(&loaded.scene);
        self.layers
            .retain(|layer, _| loaded.scene.layers.contains_key(layer));
        for &layer in loaded.scene.layers.keys() {
            self.layers.entry(layer).or_insert(true);
        }
        self.loaded = Arc::new(loaded);
        self.target = Some(DocumentTarget::File(path));
        Ok(())
    }
    fn begin_move(&mut self, pointer: Point) {
        if self.moving.is_some() || self.wiring.is_some() {
            return;
        }
        let records = self.selected_records();
        if records.is_empty() {
            return;
        }
        // Use a record's origin, not the cursor, as the shared transform anchor.
        // A shared anchor preserves connections within multi-record selections.
        let pivot = records
            .iter()
            .find_map(|&index| match &self.loaded.document.records[index].kind {
                RecordKind::Instance { at, .. } | RecordKind::Text { at, .. } => Some(*at),
                RecordKind::Wire { a, .. }
                | RecordKind::Line { a, .. }
                | RecordKind::Rectangle { a, .. } => Some(*a),
                RecordKind::Arc { center, .. } => Some(*center),
                RecordKind::Polygon { points, .. } => points.first().copied(),
                _ => None,
            })
            .unwrap_or_default();
        self.moving = Some(MoveState {
            original: Arc::clone(&self.loaded),
            restore: Arc::clone(&self.loaded),
            selection_before: self.selected.clone(),
            copying: false,
            records,
            pivot,
            grab: pointer,
            source: self.source_path(),
            at: pivot,
            rotation: 0,
            flip: false,
        });
        self.selection_box = None;
    }
    fn selected_records(&self) -> BTreeSet<usize> {
        Self::records_for(&self.loaded, &self.selected)
    }
    fn records_for(loaded: &LoadedDocument, selected: &BTreeSet<Selection>) -> BTreeSet<usize> {
        selected
            .iter()
            .filter_map(|selection| match selection {
                Selection::Instance(index) => loaded.scene.instance_records.get(index).copied(),
                Selection::Wire(record) | Selection::Primitive(record) => Some(*record),
            })
            .collect()
    }
    fn edit_state(&self) -> EditState {
        EditState {
            document: Arc::clone(&self.loaded.document),
            selection: self.selected_records(),
        }
    }
    fn apply_edit_state(&mut self, state: &EditState, paths: &LibraryPaths) -> Result<(), String> {
        if self.loaded.document.original() != state.document.original() {
            self.install_document((*state.document).clone(), paths)?;
        }
        self.layers
            .retain(|layer, _| self.loaded.scene.layers.contains_key(layer));
        self.select_records(&state.selection);
        Ok(())
    }
    fn execute_command(
        &mut self,
        operation: EditOperation,
        before: EditState,
        paths: &LibraryPaths,
    ) -> Result<(), String> {
        let mut history = std::mem::take(&mut self.history);
        let result = history.execute(Box::new(DocumentCommand::new(operation, before)), |state| {
            self.apply_edit_state(state, paths)
        });
        self.history = history;
        result
    }
    fn undo(&mut self, paths: &LibraryPaths) -> Result<(), String> {
        if self.moving.is_some() || self.selection_box.is_some() {
            self.cancel_move();
            self.selection_box = None;
            return Ok(());
        }
        self.wiring = None;
        let mut history = std::mem::take(&mut self.history);
        let result = history.undo(|state| self.apply_edit_state(state, paths));
        self.history = history;
        result
    }
    fn redo(&mut self, paths: &LibraryPaths) -> Result<(), String> {
        if self.moving.is_some() || self.wiring.is_some() || self.selection_box.is_some() {
            return Ok(());
        }
        let mut history = std::mem::take(&mut self.history);
        let result = history.redo(|state| self.apply_edit_state(state, paths));
        self.history = history;
        result
    }
    fn source_path(&self) -> PathBuf {
        match &self.target {
            Some(DocumentTarget::File(path))
            | Some(DocumentTarget::Embedded { source: path, .. }) => path.clone(),
            None => PathBuf::from("demo.sch"),
        }
    }
    fn install_document(&mut self, document: Document, paths: &LibraryPaths) -> Result<(), String> {
        let loaded = LoadedDocument::from_document_at(
            Arc::new(document),
            self.loaded.path.clone(),
            self.source_path(),
            paths.clone(),
        )?;
        for &layer in loaded.scene.layers.keys() {
            self.layers.entry(layer).or_insert(true);
        }
        self.cache = PaintCache::new(&loaded.scene);
        self.loaded = Arc::new(loaded);
        Ok(())
    }
    fn begin_copy(&mut self, pointer: Point, paths: &LibraryPaths) -> Result<(), String> {
        if self.moving.is_some() || self.wiring.is_some() {
            return Ok(());
        }
        let records = self.selected_records();
        if records.is_empty() {
            return Ok(());
        }
        let (document, copied) = self.loaded.document.with_copied_records(&records)?;
        let restore = Arc::clone(&self.loaded);
        let selection_before = self.selected.clone();
        self.install_document(document, paths)?;
        self.select_records(&copied);
        self.begin_move(pointer);
        if let Some(moving) = &mut self.moving {
            moving.restore = restore;
            moving.selection_before = selection_before;
            moving.copying = true;
        }
        Ok(())
    }
    fn select_records(&mut self, records: &BTreeSet<usize>) {
        self.selected = self
            .loaded
            .scene
            .instance_records
            .iter()
            .filter(|(_, record)| records.contains(record))
            .map(|(&instance, _)| Selection::Instance(instance))
            .chain(
                records
                    .iter()
                    .filter(|&&record| {
                        !matches!(
                            self.loaded.document.records[record].kind,
                            RecordKind::Instance { .. }
                        )
                    })
                    .map(|&record| {
                        if matches!(
                            self.loaded.document.records[record].kind,
                            RecordKind::Wire { .. }
                        ) {
                            Selection::Wire(record)
                        } else {
                            Selection::Primitive(record)
                        }
                    }),
            )
            .collect();
    }
    fn delete_selection(&mut self, paths: &LibraryPaths) -> Result<(), String> {
        if self.moving.is_some() || self.wiring.is_some() || self.selection_box.is_some() {
            return Ok(());
        }
        let records = self.selected_records();
        if records.is_empty() {
            return Ok(());
        }
        self.execute_command(EditOperation::Delete, self.edit_state(), paths)
    }
    fn wire_target(&self, pointer: Point, spacing: f64) -> (Point, bool) {
        let snapped = if spacing.is_finite() && spacing > 0. {
            Point::new(
                (pointer.x / spacing).round() * spacing,
                (pointer.y / spacing).round() * spacing,
            )
        } else {
            pointer
        };
        let port = self
            .loaded
            .scene
            .ports
            .iter()
            .copied()
            .filter(|&port| {
                (port - pointer).length() <= 8. / self.viewport.zoom
                    || (port - snapped).length() < 1e-9
            })
            .min_by(|&a, &b| (a - pointer).length().total_cmp(&(b - pointer).length()));
        if let Some(port) = port {
            return (port, true);
        }
        let wire = self
            .loaded
            .document
            .records
            .iter()
            .filter_map(|record| {
                let RecordKind::Wire { a, b, .. } = record.kind else {
                    return None;
                };
                if !self.layers.get(&1).copied().unwrap_or(true) {
                    return None;
                }
                let closest = segment_point(pointer, a, b);
                let grid_point = segment_point(snapped, a, b);
                if (grid_point - snapped).length() < 1e-9 {
                    Some((grid_point, (closest - pointer).length()))
                } else if (closest - pointer).length() <= 8. / self.viewport.zoom {
                    Some((closest, (closest - pointer).length()))
                } else {
                    None
                }
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b));
        wire.map_or((snapped, false), |(point, _)| (point, true))
    }
    fn begin_wire(&mut self, pointer: Point, spacing: f64) {
        if self.moving.is_some() || self.wiring.is_some() {
            return;
        }
        let (start, on_connection) = self.wire_target(pointer, spacing);
        self.wiring = Some(WireState {
            start,
            end: start,
            on_connection,
        });
        self.selection_box = None;
    }
    fn update_wire(&mut self, pointer: Point, spacing: f64) {
        let (end, on_connection) = self.wire_target(pointer, spacing);
        if let Some(wiring) = &mut self.wiring {
            wiring.end = end;
            wiring.on_connection = on_connection;
        }
    }
    fn place_wire(&mut self, paths: &LibraryPaths) -> Result<(), String> {
        let Some(wiring) = &self.wiring else {
            return Ok(());
        };
        let (start, end, on_connection) = (wiring.start, wiring.end, wiring.on_connection);
        if start != end {
            self.execute_command(EditOperation::Wire { start, end }, self.edit_state(), paths)?;
        }
        self.wiring = if on_connection {
            None
        } else {
            Some(WireState {
                start: end,
                end,
                on_connection: false,
            })
        };
        Ok(())
    }
    fn update_move(
        &mut self,
        pointer: Option<Point>,
        keys: &[Key],
        spacing: f64,
        paths: &LibraryPaths,
    ) -> Result<(), String> {
        let Some(moving) = &mut self.moving else {
            return Ok(());
        };
        let mut at = moving.at;
        if let Some(pointer) = pointer {
            at = moving.pivot + (pointer - moving.grab);
            if spacing.is_finite() && spacing > 0. {
                at = Point::new(
                    moving.pivot.x + ((at.x - moving.pivot.x) / spacing).round() * spacing,
                    moving.pivot.y + ((at.y - moving.pivot.y) / spacing).round() * spacing,
                );
            }
        }
        let mut rotation = moving.rotation;
        let mut flip = moving.flip;
        for key in keys {
            match key {
                Key::R => rotation = (rotation + 1) % 4,
                // World-axis reflections composed with R(rotation) * mirror-X.
                Key::X => {
                    rotation = (6 - rotation) % 4;
                    flip = !flip;
                }
                Key::Y => {
                    rotation = (4 - rotation) % 4;
                    flip = !flip;
                }
                _ => {}
            }
        }
        if (at, rotation, flip) == (moving.at, moving.rotation, moving.flip) {
            return Ok(());
        }
        let document = moving.original.document.with_transformed_records(
            &moving.records,
            moving.pivot,
            at,
            rotation,
            flip,
        )?;
        let loaded = LoadedDocument::from_document_at(
            Arc::new(document),
            moving.original.path.clone(),
            moving.source.clone(),
            paths.clone(),
        )?;
        self.cache = PaintCache::new(&loaded.scene);
        self.loaded = Arc::new(loaded);
        moving.at = at;
        moving.rotation = rotation;
        moving.flip = flip;
        Ok(())
    }
    fn cancel_move(&mut self) {
        if let Some(moving) = self.moving.take() {
            self.loaded = moving.restore;
            self.selected = moving.selection_before;
            self.cache = PaintCache::new(&self.loaded.scene);
        }
    }
    fn commit_move(&mut self, paths: &LibraryPaths) -> Result<(), String> {
        let Some(moving) = self.moving.take() else {
            return Ok(());
        };
        let before = EditState {
            document: Arc::clone(&moving.restore.document),
            selection: Self::records_for(&moving.restore, &moving.selection_before),
        };
        let placement = Placement {
            pivot: moving.pivot,
            at: moving.at,
            rotation: moving.rotation,
            flip: moving.flip,
        };
        let operation = if moving.copying {
            EditOperation::Copy(placement)
        } else {
            EditOperation::Move(placement)
        };
        let result = self.execute_command(operation, before, paths);
        if result.is_err() {
            self.moving = Some(moving);
        }
        result
    }
    fn selected_instance(&self) -> Option<usize> {
        if self.selected.len() == 1
            && let Some(Selection::Instance(index)) = self.selected.first()
        {
            Some(*index)
        } else {
            None
        }
    }
    fn selection_bounds(&self, show_hidden: bool) -> BTreeMap<Selection, Bounds> {
        let mut bounds: BTreeMap<Selection, Bounds> = BTreeMap::new();
        for item in &self.loaded.scene.items {
            if (item.hidden && !show_hidden)
                || !self.layers.get(&item.layer).copied().unwrap_or(true)
            {
                continue;
            }
            let Some(id) = item.selection() else { continue };
            if let Selection::Instance(index) = id
                && !self.loaded.scene.instance_records.contains_key(&index)
            {
                continue;
            }
            bounds
                .entry(id)
                .and_modify(|bounds| bounds.include(item.bounds))
                .or_insert(item.bounds);
        }
        bounds
    }
    fn select_box(&mut self, bounds: Bounds, show_hidden: bool, additive: bool) {
        let enclosed: BTreeSet<_> = self
            .selection_bounds(show_hidden)
            .into_iter()
            .filter(|(_, item)| bounds.contains(item.min) && bounds.contains(item.max))
            .map(|(id, _)| id)
            .collect();
        if additive {
            self.selected.extend(enclosed);
        } else {
            self.selected = enclosed;
        }
    }
    fn select_at(&mut self, point: Point, show_hidden: bool, double_click: bool, additive: bool) {
        let tolerance = 5. / self.viewport.zoom;
        let mut hits: Vec<_> = self
            .selection_bounds(show_hidden)
            .into_iter()
            .filter(|(id, bounds)| match id {
                Selection::Instance(_) => bounds.expanded(tolerance).contains(point),
                Selection::Primitive(record) => {
                    if let RecordKind::Line { a, b, .. } =
                        self.loaded.document.records[*record].kind
                    {
                        segment_distance(point, a, b) <= tolerance
                    } else {
                        bounds.expanded(tolerance).contains(point)
                    }
                }
                Selection::Wire(record) => {
                    if let RecordKind::Wire { a, b, .. } =
                        self.loaded.document.records[*record].kind
                    {
                        segment_distance(point, a, b) <= tolerance
                    } else {
                        false
                    }
                }
            })
            .collect();
        hits.sort_by(|(_, a), (_, b)| {
            (a.size().x * a.size().y).total_cmp(&(b.size().x * b.size().y))
        });
        if additive {
            if let Some((id, _)) = hits.iter().find(|(id, _)| !self.selected.contains(id)) {
                self.selected.insert(*id);
            }
            return;
        }
        let position = hits.iter().position(|(id, _)| self.selected.contains(id));
        let next = position.map_or(0, |position| {
            if double_click {
                position
            } else {
                (position + 1) % hits.len()
            }
        });
        self.selected = hits.get(next).map(|(id, _)| *id).into_iter().collect();
    }
}
fn segment_distance(point: Point, a: Point, b: Point) -> f64 {
    (point - segment_point(point, a, b)).length()
}
fn segment_point(point: Point, a: Point, b: Point) -> Point {
    let d = b - a;
    let length_squared = d.x * d.x + d.y * d.y;
    if length_squared == 0. {
        return a;
    }
    let p = point - a;
    let t = ((p.x * d.x + p.y * d.y) / length_squared).clamp(0., 1.);
    a + d * t
}
struct Completion {
    id: u64,
    push_frame: bool,
    target: Option<DocumentTarget>,
    result: Result<(Arc<LoadedDocument>, PaintCache), String>,
}
struct ExportCompletion {
    message: String,
    raw: Option<PathBuf>,
}
enum Action {
    Undo,
    Redo,
    Edit(Key),
    Open(PathBuf),
    Save,
    SaveAs,
    OpenDialog,
    Demo,
    Reload,
    ExportSpice,
    Simulate,
    OpenRaw,
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
    export: Option<mpsc::Receiver<Result<ExportCompletion, String>>>,
    export_status: Option<String>,
    waveforms: crate::waveforms::Waveforms,
    show_grid: bool,
    grid_spacing: f64,
    show_hidden: bool,
    show_libraries: bool,
    show_source: bool,
    show_diagnostics: bool,
    show_fps: bool,
    fps: f32,
    pointer: Option<Point>,
    last_pointer: Option<Point>,
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
            export: None,
            export_status: None,
            waveforms: crate::waveforms::Waveforms::default(),
            show_grid: true,
            grid_spacing: 20.,
            show_hidden: false,
            show_libraries: false,
            show_source: false,
            show_diagnostics: false,
            show_fps: false,
            fps: 0.,
            pointer: None,
            last_pointer: None,
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
        self.sync_native_menu(ctx);
    }

    #[cfg(target_os = "macos")]
    fn sync_native_menu(&self, ctx: &egui::Context) {
        if let Some(menu) = &self.native_menu {
            let keyboard =
                !ctx.wants_keyboard_input() || ctx.memory(|memory| memory.has_focus(canvas_id()));
            menu.sync_history(
                if keyboard { self.undo_label() } else { None },
                if keyboard { self.redo_label() } else { None },
            );
            menu.sync(
                self.current.is_some(),
                self.can_export(),
                self.loading.is_none()
                    && self
                        .current
                        .as_ref()
                        .is_some_and(|view| view.selected_instance().is_some()),
                !self.navigation_stack.is_empty() || self.loading.is_some(),
                [
                    self.show_grid,
                    self.show_hidden,
                    self.show_source,
                    self.show_diagnostics,
                    self.show_libraries,
                    self.show_fps,
                ],
                [
                    self.edit_enabled(Key::G),
                    self.edit_enabled(Key::W),
                    self.edit_enabled(Key::R),
                    self.edit_enabled(Key::Escape),
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
        if let Some(result) = self.export.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.export = None;
            match result {
                Ok(result) => {
                    self.export_status = Some(result.message);
                    if let Some(path) = result.raw {
                        self.waveforms.load(ctx, path);
                    }
                }
                Err(error) => {
                    self.export_status = None;
                    self.error = Some(error);
                }
            }
        }
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
    fn can_export(&self) -> bool {
        self.export.is_none()
            && self.loading.is_none()
            && self.current.as_ref().is_some_and(|view| {
                !matches!(view.target, Some(DocumentTarget::Embedded { .. }))
                    && !view.loaded.path.as_ref().is_some_and(|p| {
                        p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sym"))
                    })
            })
    }
    fn can_save(&self) -> bool {
        self.loading.is_none()
            && self.current.as_ref().is_some_and(|view| {
                view.moving.is_none() && view.wiring.is_none() && view.selection_box.is_none()
            })
    }
    fn edit_enabled(&self, key: Key) -> bool {
        let Some(view) = &self.current else {
            return false;
        };
        if self.loading.is_some() {
            return false;
        }
        let idle = view.moving.is_none() && view.wiring.is_none() && view.selection_box.is_none();
        match key {
            Key::G | Key::C | Key::Backspace => idle && !view.selected.is_empty(),
            Key::W => idle,
            Key::R | Key::X | Key::Y => view.moving.is_some(),
            Key::Escape => !idle,
            _ => false,
        }
    }
    fn undo_label(&self) -> Option<&'static str> {
        if self.loading.is_some() {
            return None;
        }
        let view = self.current.as_ref()?;
        if let Some(moving) = &view.moving {
            return Some(if moving.copying {
                "Copy preview"
            } else {
                "Move preview"
            });
        }
        if view.selection_box.is_some() {
            return Some("Selection box");
        }
        view.history
            .undo_label()
            .or_else(|| view.wiring.as_ref().map(|_| "Wire preview"))
    }
    fn redo_label(&self) -> Option<&'static str> {
        if !self.can_save() {
            return None;
        }
        self.current.as_ref()?.history.redo_label()
    }
    fn action(&mut self, ctx: &egui::Context, action: Action) {
        if matches!(action, Action::Undo | Action::Redo) {
            let undo = matches!(action, Action::Undo);
            if (if undo {
                self.undo_label()
            } else {
                self.redo_label()
            })
            .is_none()
            {
                return;
            }
            let current = self.current.as_mut().unwrap();
            let result = if undo {
                current.undo(&self.paths)
            } else {
                current.redo(&self.paths)
            };
            if let Err(error) = result {
                self.error = Some(error);
            }
            ctx.request_repaint();
            return;
        }
        if let Action::Edit(key) = action {
            if !self.edit_enabled(key) {
                return;
            }
            let current = self.current.as_mut().unwrap();
            let pointer = self
                .pointer
                .or(self.last_pointer)
                .unwrap_or(current.viewport.center);
            let result = match key {
                Key::G => {
                    current.begin_move(pointer);
                    Ok(())
                }
                Key::C => current.begin_copy(pointer, &self.paths),
                Key::Backspace => current.delete_selection(&self.paths),
                Key::W => {
                    current.begin_wire(pointer, self.grid_spacing);
                    Ok(())
                }
                Key::R | Key::X | Key::Y => {
                    current.update_move(None, &[key], self.grid_spacing, &self.paths)
                }
                Key::Escape => {
                    current.cancel_move();
                    current.wiring = None;
                    current.selection_box = None;
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                self.error = Some(error);
            }
            ctx.request_repaint();
            return;
        }
        if matches!(action, Action::Save | Action::SaveAs) && !self.can_save() {
            return;
        }
        if !matches!(action, Action::Fit)
            && let Some(current) = &mut self.current
        {
            current.cancel_move();
            current.wiring = None;
            current.selection_box = None;
        }
        match action {
            Action::Undo | Action::Redo => unreachable!("history actions handled above"),
            Action::Edit(_) => unreachable!("editing actions handled above"),
            Action::Save | Action::SaveAs => {
                let Some(current) = &mut self.current else {
                    return;
                };
                let mut path = if matches!(action, Action::Save) {
                    current.loaded.path.clone()
                } else {
                    None
                };
                if path.is_none() {
                    let filename = match &current.target {
                        Some(DocumentTarget::Embedded { symbol, .. }) => symbol.clone(),
                        _ => current.name(),
                    };
                    let mut dialog = rfd::FileDialog::new()
                        .set_title("Save schematic or symbol")
                        .add_filter("Xschem schematic or symbol", &["sch", "sym"])
                        .set_file_name(&filename);
                    if let Some(target) = &current.target {
                        dialog = dialog.set_directory(target.directory());
                    }
                    path = dialog.save_file().map(|mut path| {
                        if path.extension().is_none() {
                            path.set_extension(
                                Path::new(&filename)
                                    .extension()
                                    .unwrap_or(std::ffi::OsStr::new("sch")),
                            );
                        }
                        path
                    });
                }
                if let Some(path) = path {
                    match current.save_to(&path, &self.paths) {
                        Ok(()) => {
                            self.error = None;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                                "{} — fschem",
                                current.name()
                            )));
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            Action::Open(path) => {
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("raw"))
                {
                    self.waveforms.load(ctx, path);
                } else {
                    self.load(ctx, Some(path), true);
                }
            }
            Action::OpenRaw => self.waveforms.open_dialog(ctx),
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
            Action::ExportSpice | Action::Simulate => {
                let simulate = matches!(action, Action::Simulate);
                if !self.can_export() {
                    return;
                }
                let Some(current) = &self.current else {
                    return;
                };
                let source = current
                    .loaded
                    .path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("demo.sch"));
                let filename = source
                    .with_extension(if simulate { "raw" } else { "spice" })
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let dialog = if simulate {
                    rfd::FileDialog::new()
                        .set_title("Run ngspice — save raw results")
                        .add_filter("ngspice raw data", &["raw"])
                } else {
                    rfd::FileDialog::new().add_filter("SPICE netlist", &["spice", "sp", "cir"])
                };
                if let Some(mut path) = dialog
                    .set_directory(source.parent().unwrap_or(Path::new(".")))
                    .set_file_name(filename)
                    .save_file()
                {
                    if simulate && path.extension().is_none() {
                        path.set_extension("raw");
                    }
                    let document = Arc::clone(&current.loaded.document);
                    let paths = self.paths.clone();
                    let ctx = ctx.clone();
                    let (tx, rx) = mpsc::channel();
                    self.export = Some(rx);
                    self.export_status = Some(
                        if simulate {
                            "Generating netlist and running ngspice…"
                        } else {
                            "Exporting SPICE netlist…"
                        }
                        .into(),
                    );
                    self.error = None;
                    std::thread::spawn(move || {
                        let result = if simulate {
                            fschem::simulation::run(&document, &source, paths, &path).map(|files| {
                                let mut message = format!(
                                    "Simulation complete: {} · Netlist: {} · Log: {}",
                                    files.raw.display(),
                                    files.netlist.display(),
                                    files.log.display()
                                );
                                if !files.warnings.is_empty() {
                                    message.push_str(&format!(
                                        "\n{} ngspice startup notice(s); see log. {}",
                                        files.warnings.len(),
                                        files.warnings[0]
                                    ));
                                }
                                ExportCompletion {
                                    message,
                                    raw: Some(files.raw),
                                }
                            })
                        } else {
                            fschem::netlist::spice(&document, &source, paths)
                                .and_then(|deck| fschem::netlist::write_spice(&path, &deck))
                                .map(|()| ExportCompletion {
                                    message: format!(
                                        "SPICE netlist exported to {}",
                                        path.display()
                                    ),
                                    raw: None,
                                })
                        };
                        let _ = tx.send(result);
                        ctx.request_repaint();
                    });
                }
            }
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
                    .selected_instance()
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
        if self.show_fps {
            let dt = ctx.input(|input| input.unstable_dt);
            if dt > 0. && dt.is_finite() && dt < 1. {
                let fps = 1. / dt;
                self.fps = if self.fps > 0. {
                    self.fps * 0.9 + fps * 0.1
                } else {
                    fps
                };
            }
            ctx.request_repaint();
        }
        let mut actions = Vec::new();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.native_menu {
            use crate::macos_menu::Command;
            for command in menu.commands() {
                match command {
                    Command::Open => actions.push(Action::OpenDialog),
                    Command::Undo => actions.push(Action::Undo),
                    Command::Redo => actions.push(Action::Redo),
                    Command::Edit(key) => actions.push(Action::Edit(key)),
                    Command::Save => actions.push(Action::Save),
                    Command::SaveAs => actions.push(Action::SaveAs),
                    Command::Demo => actions.push(Action::Demo),
                    Command::Reload => actions.push(Action::Reload),
                    Command::ExportSpice => actions.push(Action::ExportSpice),
                    Command::Fit => actions.push(Action::Fit),
                    Command::Grid => self.show_grid = !self.show_grid,
                    Command::Hidden => self.show_hidden = !self.show_hidden,
                    Command::Source => self.show_source = !self.show_source,
                    Command::Diagnostics => self.show_diagnostics = !self.show_diagnostics,
                    Command::Libraries => self.show_libraries = true,
                    Command::Fps => self.show_fps = !self.show_fps,
                    Command::Symbol => actions.push(Action::Descend(Descent::Symbol)),
                    Command::Schematic => actions.push(Action::Descend(Descent::Schematic)),
                    Command::Back => actions.push(Action::Back),
                }
            }
        }
        #[cfg(target_os = "macos")]
        let history_shortcuts = self.native_menu.is_none();
        #[cfg(not(target_os = "macos"))]
        let history_shortcuts = true;
        if history_shortcuts
            && (!ctx.wants_keyboard_input() || ctx.memory(|memory| memory.has_focus(canvas_id())))
        {
            ctx.input_mut(|input| {
                if input.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    Key::Z,
                )) {
                    actions.push(Action::Redo);
                } else if input.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::COMMAND,
                    Key::Z,
                )) {
                    actions.push(Action::Undo);
                }
                #[cfg(not(target_os = "macos"))]
                if input
                    .consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, Key::Y))
                {
                    actions.push(Action::Redo);
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        ctx.input_mut(|input| {
            if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                Key::S,
            )) {
                actions.push(Action::SaveAs);
            } else if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                Key::S,
            )) {
                actions.push(Action::Save);
            }
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
                if input.modifiers == egui::Modifiers::SHIFT && input.key_pressed(Key::E) {
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
                    if ui
                        .add_enabled(
                            self.can_save(),
                            egui::Button::new("Save     Ctrl/Command-S"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Save);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.can_save(),
                            egui::Button::new("Save As…     Ctrl/Command-Shift-S"),
                        )
                        .clicked()
                    {
                        actions.push(Action::SaveAs);
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
                    if ui
                        .add_enabled(
                            self.can_export(),
                            egui::Button::new("Export SPICE netlist…"),
                        )
                        .clicked()
                    {
                        actions.push(Action::ExportSpice);
                        ui.close();
                    }
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let undo = self.undo_label();
                    let redo = self.redo_label();
                    if ui
                        .add_enabled(
                            undo.is_some(),
                            egui::Button::new(
                                undo.map_or("Undo".into(), |label| format!("Undo {label}")),
                            )
                            .shortcut_text("Ctrl-Z"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Undo);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            redo.is_some(),
                            egui::Button::new(
                                redo.map_or("Redo".into(), |label| format!("Redo {label}")),
                            )
                            .shortcut_text("Ctrl-Shift-Z / Ctrl-Y"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Redo);
                        ui.close();
                    }
                    ui.separator();
                    for (key, label, shortcut) in [
                        (Key::G, "Move", "G"),
                        (Key::C, "Copy", "C"),
                        (Key::Backspace, "Delete", "Backspace"),
                        (Key::W, "Add wire", "W"),
                        (Key::R, "Rotate 90°", "R"),
                        (Key::X, "Mirror across X axis", "X"),
                        (Key::Y, "Mirror across Y axis", "Y"),
                        (Key::Escape, "Cancel", "Esc"),
                    ] {
                        if ui
                            .add_enabled(
                                self.edit_enabled(key),
                                egui::Button::new(label).shortcut_text(shortcut),
                            )
                            .clicked()
                        {
                            actions.push(Action::Edit(key));
                            ui.close();
                        }
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
                    ui.checkbox(&mut self.show_fps, "FPS counter");
                });
                ui.menu_button("Navigate", |ui| {
                    let selected = self.loading.is_none()
                        && self
                            .current
                            .as_ref()
                            .is_some_and(|view| view.selected_instance().is_some());
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
                    ui.weak("Xschem files");
                });
            });
        });
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open…").clicked() {
                    actions.push(Action::OpenDialog);
                }
                if ui.add_enabled(self.can_save(), egui::Button::new("Save")).on_hover_text("Save schematic or symbol (Cmd/Ctrl-S)").clicked() {
                    actions.push(Action::Save);
                }
                if ui.button("Open raw…").on_hover_text("Plot ngspice raw data in a separate waveform window").clicked() {
                    actions.push(Action::OpenRaw);
                }
                if ui.add_enabled(self.can_export(), egui::Button::new("Run ngspice…"))
                    .on_hover_text("Generate a SPICE netlist, run ngspice, and save .raw results plus .spice and .log files")
                    .clicked() {
                    actions.push(Action::Simulate);
                }
                if self.export.is_some() { ui.spinner(); }
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
            if let Some(status) = &self.export_status {
                ui.label(status);
            }
            ui.horizontal(|ui| {
                if let Some(current) = &self.current {
                    ui.label(if current.modified() {
                        "Modified"
                    } else if current.loaded.path.is_some() {
                        "Saved"
                    } else {
                        "Not saved"
                    });
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
                    if self.show_fps && self.fps > 0. {
                        ui.monospace(format!("{:.0} FPS", self.fps));
                    }
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
                    if current.moving.is_some() {
                        ui.colored_label(Color32::from_rgb(255, 185, 65), if current.moving.as_ref().is_some_and(|moving| moving.copying) {
                            "Copying · Left-click to place · Esc to cancel"
                        } else { "Moving · Left-click to place · Esc to cancel" });
                        ui.weak("R: rotate 90° · X/Y: mirror across axis");
                    } else if current.wiring.is_some() {
                        ui.colored_label(Color32::from_rgb(255, 185, 65), "Wiring · Click to add a segment");
                        ui.weak("Click a port or wire to finish · Esc to stop");
                    } else if !current.selected.is_empty() {
                        ui.weak(format!("{} selected · G: move · C: copy · W: wire", current.selected.len()));
                    }
                    if let Some(instance) = current.selected_instance().and_then(|i| loaded.scene.instances.get(i)) {
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
                    } else { ui.weak("Click a component or wire to select it. Left-drag to box select. Hold Shift to add to the selection. G: move · C: copy · W: wire · i: symbol · e: schematic · Shift-E: up."); }
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
                    self.pointer = response.hover_pos().map(|pos| canvas.world(pos));
                    if self.pointer.is_some() { self.last_pointer = self.pointer; }
                    let keyboard = !ctx.wants_keyboard_input() || response.has_focus();
                    let keys = ui.input(|input| {
                        if keyboard && input.modifiers == egui::Modifiers::NONE {
                            input.events.iter().filter_map(|event| match event {
                                egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                                _ => None,
                            }).collect::<Vec<_>>()
                        } else { Vec::new() }
                    });
                    if self.loading.is_none() && !keys.contains(&Key::Escape)
                        && current.selection_box.is_none()
                        && let Some(pointer) = self.pointer {
                        if keys.contains(&Key::G) {
                            current.begin_move(pointer);
                            response.request_focus();
                        } else if keys.contains(&Key::C) {
                            if let Err(error) = current.begin_copy(pointer, &self.paths) { self.error = Some(error); }
                            response.request_focus();
                        } else if keys.contains(&Key::W) {
                            current.begin_wire(pointer, self.grid_spacing);
                            response.request_focus();
                        }
                    }
                    let was_moving = current.moving.is_some();
                    let was_wiring = current.wiring.is_some();
                    if self.loading.is_none() && keys.contains(&Key::Backspace)
                        && let Err(error) = current.delete_selection(&self.paths) { self.error = Some(error); }
                    if keys.contains(&Key::Escape) {
                        current.cancel_move();
                        current.wiring = None;
                        current.selection_box = None;
                    } else if was_moving {
                        match current.update_move(self.pointer, &keys, self.grid_spacing, &self.paths) {
                            Ok(()) => {
                                if response.clicked_by(PointerButton::Primary)
                                    && let Err(error) = current.commit_move(&self.paths) { self.error = Some(error); }
                            }
                            Err(error) => self.error = Some(error),
                        }
                    } else if was_wiring && let Some(pointer) = self.pointer {
                        current.update_wire(pointer, self.grid_spacing);
                        if response.clicked_by(PointerButton::Primary)
                            && let Err(error) = current.place_wire(&self.paths) { self.error = Some(error); }
                    }
                    if !was_moving && !was_wiring && !keys.contains(&Key::Escape) {
                        if response.drag_started_by(PointerButton::Primary) {
                            current.selection_box = ui.input(|input| input.pointer.press_origin())
                                .map(|pos| canvas.world(pos));
                            response.request_focus();
                        }
                        if response.drag_stopped_by(PointerButton::Primary)
                            && let Some(start) = current.selection_box.take()
                            && let Some(end) = ui.input(|input| input.pointer.interact_pos()) {
                            current.select_box(Bounds::new(start, canvas.world(end)), self.show_hidden, ui.input(|input| input.modifiers.shift));
                        }
                    }
                    if self.show_grid {
                        canvas.grid(self.grid_spacing);
                    }
                    canvas.scene(
                        &current.loaded.scene,
                        &current.cache,
                        &current.layers,
                        self.show_hidden,
                        &current.selected,
                    );
                    if let Some(wiring) = &current.wiring {
                        canvas.wire_preview(wiring.start, wiring.end, wiring.on_connection);
                    }
                    if let Some(start) = current.selection_box
                        && let Some(end) = ui.input(|input| input.pointer.interact_pos()) {
                        let selection_rect = egui::Rect::from_two_pos(canvas.screen(start), end);
                        painter.rect_filled(selection_rect, 0., Color32::from_rgba_unmultiplied(255, 185, 65, 24));
                        painter.rect_stroke(selection_rect, 0., egui::Stroke::new(1., Color32::from_rgb(255, 185, 65)), egui::StrokeKind::Inside);
                    }
                    self.pointer = response.hover_pos().map(|pos| canvas.world(pos));
                    if response.clicked_by(PointerButton::Primary) && !was_moving && !was_wiring {
                        response.request_focus();
                        if let Some(point) = response.interact_pointer_pos().map(|p| canvas.world(p)) {
                            current.select_at(point, self.show_hidden, response.double_clicked_by(PointerButton::Primary), ui.input(|input| input.modifiers.shift));
                        }
                        if response.double_clicked_by(PointerButton::Primary) && !ui.input(|input| input.modifiers.shift) && current.selected_instance().is_some() {
                            actions.push(Action::Descend(Descent::Symbol));
                        }
                        ctx.request_repaint();
                    }
                    response.on_hover_cursor(if current.moving.is_some() {
                        egui::CursorIcon::Grabbing
                    } else if current.wiring.is_some() {
                        egui::CursorIcon::Crosshair
                    } else {
                        egui::CursorIcon::Default
                    });
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
                            "Click to select · Left-drag to box select · G to move · Middle/right-drag to pan · Wheel to zoom",
                        );
                    });
                }
            });
        self.windows(ctx, &mut actions);
        for action in actions {
            self.action(ctx, action);
        }
        #[cfg(target_os = "macos")]
        self.sync_native_menu(ctx);
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
            egui::Window::new("Operation failed")
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
        self.waveforms.show(ctx);
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
    fn selection_view() -> DocumentView {
        let source = "v {xschem file_version=1.3}\nC {test.sym} 0 0 0 0 {name=X1}\n[\nL 4 -10 -10 10 10 {}\n]\nN0 0 100 0 {lab=signal}\nN 0 0 100 100 {}\nN -100 50 150 50 {}\nN 200 200 200 200 {}\n";
        let loaded = Arc::new(
            LoadedDocument::from_document(
                Arc::new(fschem::document::Document::parse(source).unwrap()),
                None,
                LibraryPaths::default(),
            )
            .unwrap(),
        );
        let mut view = DocumentView::new(Arc::clone(&loaded), PaintCache::new(&loaded.scene));
        view.viewport.zoom = 1.;
        view.fit = false;
        view
    }
    #[test]
    fn history_groups_placements_ignores_cancelled_previews_and_undoes_wire_clicks() {
        let mut view = selection_view();
        let paths = LibraryPaths::default();
        view.selected = BTreeSet::from([Selection::Instance(0), Selection::Wire(3)]);
        let original = view.loaded.document.original().to_owned();
        let original_selection = view.selected.clone();
        view.begin_move(Point::new(5., 7.));
        view.update_move(Some(Point::new(45., 47.)), &[Key::R, Key::X], 20., &paths)
            .unwrap();
        view.update_move(Some(Point::new(65., 67.)), &[Key::Y], 20., &paths)
            .unwrap();
        assert!(
            view.history.undo_label().is_none(),
            "pointer previews are not commands"
        );
        view.commit_move(&paths).unwrap();
        let moved = view.loaded.document.original().to_owned();
        view.undo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), original);
        assert_eq!(view.selected, original_selection);
        assert!(
            view.history.undo_label().is_none(),
            "the entire placement is one command"
        );
        assert!(!view.modified());
        view.begin_copy(Point::default(), &paths).unwrap();
        view.update_move(Some(Point::new(40., 60.)), &[Key::R], 20., &paths)
            .unwrap();
        view.undo(&paths).unwrap(); // Undo cancels an unplaced copy first.
        assert!(view.moving.is_none());
        assert_eq!(view.loaded.document.original(), original);
        assert_eq!(view.history.redo_label(), Some("Move"));
        view.begin_move(Point::default());
        view.update_move(Some(Point::new(40., 60.)), &[], 20., &paths)
            .unwrap();
        view.update_move(
            Some(Point::default()),
            &[Key::R, Key::R, Key::R, Key::R],
            20.,
            &paths,
        )
        .unwrap();
        view.commit_move(&paths).unwrap();
        assert_eq!(
            view.history.redo_label(),
            Some("Move"),
            "no-op placement preserves redo"
        );
        view.redo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), moved);
        view.undo(&paths).unwrap();
        view.begin_wire(Point::new(-40., -40.), 20.);
        view.update_wire(Point::new(0., -40.), 20.);
        view.place_wire(&paths).unwrap();
        let first = view.loaded.document.original().to_owned();
        view.update_wire(Point::new(20., -20.), 20.);
        view.place_wire(&paths).unwrap();
        let second = view.loaded.document.original().to_owned();
        assert!(view.wiring.is_some());
        assert!(
            view.history.redo_label().is_none(),
            "a new committed edit starts a new branch"
        );
        view.undo(&paths).unwrap();
        assert!(view.wiring.is_none());
        assert_eq!(view.loaded.document.original(), first);
        view.undo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), original);
        view.redo(&paths).unwrap();
        view.redo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), second);
    }
    #[test]
    fn undo_redo_shortcuts_use_the_current_documents_history() {
        fn shortcut(viewer: &mut Viewer, ctx: &egui::Context, shift: bool) {
            let modifiers = egui::Modifiers::COMMAND
                | if shift {
                    egui::Modifiers::SHIFT
                } else {
                    egui::Modifiers::NONE
                };
            for pressed in [true, false] {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1200., 800.),
                        )),
                        modifiers,
                        events: vec![egui::Event::Key {
                            key: Key::Z,
                            physical_key: Some(Key::Z),
                            pressed,
                            repeat: false,
                            modifiers,
                        }],
                        ..Default::default()
                    },
                    |ctx| viewer.show(ctx),
                );
            }
        }
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
        let mut parent = selection_view();
        let parent_original = parent.loaded.document.original().to_owned();
        parent.selected = BTreeSet::from([Selection::Wire(3)]);
        parent.delete_selection(&viewer.paths).unwrap();
        let parent_edited = parent.loaded.document.original().to_owned();
        viewer.navigation_stack.push(parent);
        let mut child = selection_view();
        let child_original = child.loaded.document.original().to_owned();
        child.selected = BTreeSet::from([Selection::Instance(0)]);
        child.delete_selection(&viewer.paths).unwrap();
        let child_edited = child.loaded.document.original().to_owned();
        viewer.current = Some(child);
        ctx.memory_mut(|memory| memory.request_focus(canvas_id()));
        shortcut(&mut viewer, &ctx, false);
        assert_eq!(
            viewer.current.as_ref().unwrap().loaded.document.original(),
            child_original
        );
        assert_eq!(
            viewer.navigation_stack[0].loaded.document.original(),
            parent_edited
        );
        shortcut(&mut viewer, &ctx, true);
        assert_eq!(
            viewer.current.as_ref().unwrap().loaded.document.original(),
            child_edited
        );
        viewer.action(&ctx, Action::Back);
        shortcut(&mut viewer, &ctx, false);
        assert_eq!(
            viewer.current.as_ref().unwrap().loaded.document.original(),
            parent_original
        );
        shortcut(&mut viewer, &ctx, true);
        assert_eq!(
            viewer.current.as_ref().unwrap().loaded.document.original(),
            parent_edited
        );
    }
    #[test]
    fn wiring_to_existing_segments_finishes_at_an_exact_connection() {
        let mut view = selection_view();
        let paths = LibraryPaths::default();
        view.begin_wire(Point::new(60., -40.), 20.);
        view.update_wire(Point::new(61., 2.), 20.);
        assert_eq!(view.wiring.as_ref().unwrap().end, Point::new(60., 0.));
        assert!(view.wiring.as_ref().unwrap().on_connection);
        view.place_wire(&paths).unwrap();
        assert!(view.wiring.is_none());
        let wires: Vec<_> = view
            .loaded
            .document
            .records
            .iter()
            .filter_map(|record| match record.kind {
                RecordKind::Wire { a, b, .. } => Some((a, b)),
                _ => None,
            })
            .collect();
        assert!(fschem::scene::junction_points(&wires).contains(&Point::new(60., 0.)));
        view.begin_wire(Point::new(180., 100.), 20.);
        view.update_wire(Point::new(79., 82.), 20.);
        assert_eq!(view.wiring.as_ref().unwrap().end, Point::new(80., 80.));
        view.place_wire(&paths).unwrap();
        assert!(view.wiring.is_none());
        assert!(
            matches!(view.loaded.document.records.last().unwrap().kind, RecordKind::Wire { b, .. } if b == Point::new(80., 80.))
        );
        for a in [Point::new(7., 13.), Point::new(-25., -31.)] {
            for b in [
                Point::new(100., 40.),
                Point::new(-50., 70.),
                Point::new(70., -40.),
                Point::new(-60., -50.),
            ] {
                let routed = Document::parse("").unwrap().with_wire(a, b).unwrap();
                let mut previous = a;
                for record in routed.records {
                    let RecordKind::Wire { a, b, .. } = record.kind else {
                        panic!("wire expected")
                    };
                    assert_eq!(a, previous);
                    let delta = b - a;
                    assert!(
                        delta.x == 0.
                            || delta.y == 0.
                            || (delta.x.abs() - delta.y.abs()).abs() < 1e-9
                    );
                    previous = b;
                }
                assert_eq!(previous, b);
            }
        }
    }
    #[test]
    fn symbol_editing_deletion_and_saving_round_trip() {
        let paths = LibraryPaths::default();
        let source = "v {xschem file_version=1.3}\nK {type=subcircuit}\nL 4 -20 0 20 0 {}\nB 5 -2 -2 2 2 {name=P1 dir=in}\nA 4 50 0 10 0 180 {}\nP 4 3 0 20 20 20 20 40 {}\nT {label} 50 50 0 0 .2 .2 {}\n";
        let loaded = Arc::new(
            LoadedDocument::from_document(
                Arc::new(Document::parse(source).unwrap()),
                None,
                paths.clone(),
            )
            .unwrap(),
        );
        let mut view = DocumentView::new(Arc::clone(&loaded), PaintCache::new(&loaded.scene));
        view.select_at(Point::default(), false, false, false);
        assert!(matches!(
            view.selected.first(),
            Some(Selection::Primitive(_))
        ));
        view.select_box(
            Bounds::new(Point::new(-500., -500.), Point::new(500., 500.)),
            false,
            false,
        );
        assert_eq!(view.selected.len(), 5);
        view.begin_move(Point::default());
        view.update_move(Some(Point::new(40., 60.)), &[Key::R], 20., &paths)
            .unwrap();
        view.commit_move(&paths).unwrap();
        assert!(view.modified());
        let transformed_port = view.loaded.scene.ports[0];
        assert_ne!(transformed_port, Point::default());
        let moved = Arc::clone(&view.loaded);
        view.begin_copy(Point::default(), &paths).unwrap();
        view.update_move(Some(Point::new(100., 0.)), &[], 20., &paths)
            .unwrap();
        view.commit_move(&paths).unwrap();
        assert_eq!(view.selected.len(), 5);
        assert_eq!(view.loaded.scene.ports.len(), 2);
        assert!(view.loaded.document.records.iter().any(|r| matches!(&r.kind, RecordKind::Rectangle { properties, .. } if properties.get("name") == Some("P2"))));
        view.delete_selection(&paths).unwrap();
        assert!(view.selected.is_empty());
        assert_eq!(view.loaded.document.original(), moved.document.original());
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("symbol-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("edited.sym");
        view.save_to(&path, &paths).unwrap();
        assert!(!view.modified());
        assert_eq!(view.name(), "edited.sym");
        let reopened = LoadedDocument::open(&path, paths.clone()).unwrap();
        assert_eq!(
            reopened.document.original(),
            view.loaded.document.original()
        );
        assert_eq!(reopened.scene.ports, vec![transformed_port]);
        view.selected = BTreeSet::from([Selection::Primitive(3)]);
        view.delete_selection(&paths).unwrap();
        assert!(view.loaded.scene.ports.is_empty());
        assert!(view.modified());
        let saved = view.saved.original().to_owned();
        assert!(
            view.save_to(&dir.join("missing").join("failed.sym"), &paths)
                .is_err()
        );
        assert_eq!(view.saved.original(), saved);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        view.save_to(&path, &paths).unwrap();
        assert!(!view.modified());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            view.loaded.document.original()
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "saving must clean up temporary files"
        );
        let saved_contents = view.loaded.document.original().to_owned();
        view.undo(&paths).unwrap();
        assert!(view.modified());
        assert_eq!(view.loaded.scene.ports.len(), 1);
        assert_eq!(view.name(), "edited.sym");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            saved_contents,
            "undo never changes the file on disk"
        );
        view.redo(&paths).unwrap();
        assert!(
            !view.modified(),
            "redoing to the saved state clears the modified marker"
        );
        let new_path = dir.join("saved-as.sym");
        view.save_to(&new_path, &paths).unwrap();
        view.undo(&paths).unwrap();
        assert_eq!(
            view.name(),
            "saved-as.sym",
            "history must retain the current Save As destination"
        );
        assert_eq!(view.loaded.path.as_deref(), Some(new_path.as_path()));
        view.redo(&paths).unwrap();
        assert!(!view.modified());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn edit_menu_actions_obey_tool_state_and_delete_the_selection() {
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
        let mut view = selection_view();
        view.selected = BTreeSet::from([Selection::Instance(0), Selection::Wire(3)]);
        viewer.current = Some(view);
        viewer.last_pointer = Some(Point::default());
        assert!(viewer.edit_enabled(Key::G));
        assert!(!viewer.edit_enabled(Key::R));
        viewer.action(&ctx, Action::Edit(Key::C));
        assert!(viewer.edit_enabled(Key::R));
        assert!(!viewer.edit_enabled(Key::Backspace));
        assert!(!viewer.can_save());
        viewer.action(&ctx, Action::Edit(Key::R));
        viewer.action(&ctx, Action::Edit(Key::Escape));
        assert_eq!(
            viewer
                .current
                .as_ref()
                .unwrap()
                .loaded
                .scene
                .instances
                .len(),
            1
        );
        viewer.action(&ctx, Action::Edit(Key::Backspace));
        let view = viewer.current.as_ref().unwrap();
        assert!(view.selected.is_empty());
        assert!(view.loaded.scene.instances.is_empty());
        assert!(view.loaded.document.diagnostics.is_empty());
        assert!(!viewer.edit_enabled(Key::G));
        assert!(viewer.can_save());
        assert!(viewer.navigation_stack.is_empty());
    }
    #[test]
    fn copying_a_group_places_duplicates_and_cancel_restores_the_original_selection() {
        let mut view = selection_view();
        view.selected = BTreeSet::from([Selection::Instance(0), Selection::Wire(3)]);
        let selection = view.selected.clone();
        let original = Arc::clone(&view.loaded);
        let paths = LibraryPaths::default();
        view.begin_copy(Point::default(), &paths).unwrap();
        assert_eq!(view.loaded.scene.instances.len(), 2);
        assert!(
            view.loaded.scene.diagnostics.is_empty(),
            "copies reuse embedded definitions"
        );
        assert!(view.moving.as_ref().unwrap().copying);
        view.update_move(Some(Point::new(40., 60.)), &[Key::R], 20., &paths)
            .unwrap();
        view.cancel_move();
        assert!(Arc::ptr_eq(&view.loaded, &original));
        assert_eq!(view.selected, selection);

        view.begin_copy(Point::default(), &paths).unwrap();
        view.update_move(Some(Point::new(40., 60.)), &[], 20., &paths)
            .unwrap();
        view.commit_move(&paths).unwrap();
        assert_eq!(view.loaded.scene.instances[0].at, Point::default());
        assert_eq!(view.loaded.scene.instances[1].at, Point::new(40., 60.));
        assert_eq!(
            view.loaded.scene.instances[1].properties.get("name"),
            Some("X2")
        );
        assert_eq!(
            view.selected,
            BTreeSet::from([Selection::Instance(1), Selection::Wire(8)])
        );
        assert!(
            view.loaded
                .document
                .original()
                .starts_with(original.document.original())
        );
        let committed = Arc::clone(&view.loaded);
        let selection = view.selected.clone();
        view.begin_copy(Point::default(), &paths).unwrap();
        assert_eq!(
            view.loaded.scene.instances[2].properties.get("name"),
            Some("X3")
        );
        view.cancel_move();
        assert!(Arc::ptr_eq(&view.loaded, &committed));
        assert_eq!(view.selected, selection);
    }
    #[test]
    fn c_and_w_shortcuts_place_copies_and_draw_until_a_port_or_escape() {
        fn frame(viewer: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| viewer.show(ctx),
            );
        }
        fn key(viewer: &mut Viewer, ctx: &egui::Context, key: Key) {
            frame(
                viewer,
                ctx,
                vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            frame(
                viewer,
                ctx,
                vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        fn click(viewer: &mut Viewer, ctx: &egui::Context, pos: egui::Pos2) {
            frame(
                viewer,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            frame(
                viewer,
                ctx,
                vec![egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
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
        let loaded = Arc::new(LoadedDocument::from_document(Arc::new(Document::parse("v {xschem file_version=1.3}\nC {pin.sym} 200 100 1 1 {name=X1}\n[\nB 5 5 11 9 15 {name=P dir=in}\n]\n").unwrap()), None, LibraryPaths::default()).unwrap());
        assert_eq!(loaded.scene.ports, vec![Point::new(187., 93.)]);
        let mut view = DocumentView::new(Arc::clone(&loaded), PaintCache::new(&loaded.scene));
        view.fit = false;
        view.viewport.zoom = 1.;
        view.selected = BTreeSet::from([Selection::Instance(0)]);
        viewer.current = Some(view);
        frame(&mut viewer, &ctx, vec![]);
        let center = ctx.read_response(canvas_id()).unwrap().rect.center();
        frame(&mut viewer, &ctx, vec![egui::Event::PointerMoved(center)]);
        key(&mut viewer, &ctx, Key::C);
        assert!(
            viewer
                .current
                .as_ref()
                .unwrap()
                .moving
                .as_ref()
                .unwrap()
                .copying
        );
        click(&mut viewer, &ctx, center + egui::vec2(-40., 0.));
        assert_eq!(
            viewer
                .current
                .as_ref()
                .unwrap()
                .loaded
                .scene
                .instances
                .len(),
            2
        );
        assert!(viewer.current.as_ref().unwrap().moving.is_none());
        frame(&mut viewer, &ctx, vec![egui::Event::PointerMoved(center)]);
        key(&mut viewer, &ctx, Key::W);
        click(&mut viewer, &ctx, center + egui::vec2(40., 0.));
        assert!(viewer.current.as_ref().unwrap().wiring.is_some());
        click(&mut viewer, &ctx, center + egui::vec2(40., 60.));
        assert!(viewer.current.as_ref().unwrap().wiring.is_some());
        let selection = viewer.current.as_ref().unwrap().selected.clone();
        click(&mut viewer, &ctx, center + egui::vec2(189., 94.));
        let view = viewer.current.as_ref().unwrap();
        assert!(view.wiring.is_none());
        assert_eq!(
            view.selected, selection,
            "port click must not select or descend"
        );
        let wires: Vec<_> = view
            .loaded
            .document
            .records
            .iter()
            .filter_map(|record| match record.kind {
                RecordKind::Wire { a, b, .. } => Some((a, b)),
                _ => None,
            })
            .collect();
        assert_eq!(
            wires,
            vec![
                (Point::default(), Point::new(40., 0.)),
                (Point::new(40., 0.), Point::new(40., 60.)),
                (Point::new(40., 60.), Point::new(73., 93.)),
                (Point::new(73., 93.), Point::new(187., 93.))
            ]
        );
        key(&mut viewer, &ctx, Key::W); // Begin another wire on the port.
        assert!(viewer.current.as_ref().unwrap().wiring.is_some());
        click(&mut viewer, &ctx, center + egui::vec2(240., 120.));
        let committed = Arc::clone(&viewer.current.as_ref().unwrap().loaded);
        frame(
            &mut viewer,
            &ctx,
            vec![egui::Event::PointerMoved(center + egui::vec2(280., 160.))],
        );
        key(&mut viewer, &ctx, Key::Escape);
        let view = viewer.current.as_ref().unwrap();
        assert!(view.wiring.is_none());
        assert!(
            Arc::ptr_eq(&view.loaded, &committed),
            "Escape keeps placed segments and discards the preview"
        );
        assert!(viewer.navigation_stack.is_empty());
    }
    #[test]
    fn additive_clicks_and_boxes_preserve_the_existing_selection() {
        let mut view = selection_view();
        view.select_at(Point::new(-8., -8.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Instance(0)]));
        view.select_at(Point::new(60., 0.), false, false, true);
        assert_eq!(
            view.selected,
            BTreeSet::from([Selection::Instance(0), Selection::Wire(3)])
        );
        view.select_at(Point::new(50., 50.), false, false, true);
        view.select_at(Point::new(50., 50.), false, false, true);
        assert_eq!(
            view.selected.len(),
            4,
            "repeated Shift-clicks add overlapping items"
        );
        let selection = view.selected.clone();
        view.select_at(Point::new(50., 50.), false, false, true);
        view.select_at(Point::new(300., 300.), false, false, true);
        assert_eq!(
            view.selected, selection,
            "selected items and empty space do not clear the group"
        );
        view.select_box(
            Bounds::new(Point::new(190., 190.), Point::new(210., 210.)),
            false,
            true,
        );
        assert_eq!(view.selected.len(), 5);
        view.select_box(
            Bounds::new(Point::new(300., 300.), Point::new(310., 310.)),
            false,
            true,
        );
        assert_eq!(view.selected.len(), 5);
        view.select_at(Point::new(60., 0.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(3)]));
        view.select_at(Point::new(300., 300.), false, false, false);
        assert!(view.selected.is_empty());
    }
    #[test]
    fn wire_picking_uses_segment_distance_and_cycles_overlaps() {
        let mut view = selection_view();
        view.select_at(Point::new(60., 2.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(3)]));
        view.select_at(Point::new(60., 2.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(3)]));
        view.select_at(Point::new(70., 20.), false, false, false);
        assert!(
            view.selected.is_empty(),
            "diagonal bounding box is not the wire"
        );
        view.select_at(Point::new(50., 50.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(5)]));
        view.select_at(Point::new(50., 50.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(4)]));
        view.select_at(Point::new(50., 50.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(5)]));
        view.select_at(Point::new(201., 200.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(6)]));
        view.layers.insert(1, false);
        view.select_at(Point::new(60., 0.), false, false, false);
        assert!(view.selected.is_empty());
    }
    #[test]
    fn box_selection_moves_and_transforms_connected_wires_and_components_together() {
        let mut view = selection_view();
        let original = Arc::clone(&view.loaded);
        let bounds = Bounds::new(Point::new(120., 120.), Point::new(-20., -20.));
        view.select_box(bounds, false, false);
        let selection = BTreeSet::from([
            Selection::Instance(0),
            Selection::Wire(3),
            Selection::Wire(4),
        ]);
        assert_eq!(
            view.selected, selection,
            "crossing but not enclosed wire is excluded"
        );
        let paths = LibraryPaths::default();
        view.begin_move(Point::default());
        view.update_move(Some(Point::new(43., 57.)), &[], 20., &paths)
            .unwrap();
        assert_eq!(view.loaded.scene.instances[0].at, Point::new(40., 60.));
        assert!(
            view.loaded
                .document
                .original()
                .contains("N40 60 140 60 {lab=signal}")
        );
        for key in [Key::R, Key::X, Key::Y] {
            view.update_move(None, &[key], 20., &paths).unwrap();
            let component = &view.loaded.scene.instances[0];
            for record in [3, 4] {
                let RecordKind::Wire { a, .. } = view.loaded.document.records[record].kind else {
                    panic!("wire expected")
                };
                assert_eq!(
                    a, component.at,
                    "transforms keep selected endpoints connected"
                );
            }
        }
        assert!(
            view.loaded
                .document
                .original()
                .contains("N -100 50 150 50 {}")
        );
        view.cancel_move();
        assert!(Arc::ptr_eq(&view.loaded, &original));
        assert_eq!(view.selected, selection);
        view.selected = BTreeSet::from([Selection::Wire(3)]);
        view.begin_move(Point::new(50., 0.));
        view.update_move(Some(Point::new(90., 60.)), &[], 20., &paths)
            .unwrap();
        view.commit_move(&paths).unwrap();
        assert!(
            view.loaded
                .document
                .original()
                .contains("N40 60 140 60 {lab=signal}")
        );
        assert_eq!(view.loaded.scene.instances[0].at, Point::default());
        view.select_at(Point::new(90., 60.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(3)]));
    }
    #[test]
    fn left_drag_selects_a_group_and_grab_click_places_it() {
        fn frame(viewer: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| viewer.show(ctx),
            );
        }
        fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }
        }
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
        viewer.current = Some(selection_view());
        frame(&mut viewer, &ctx, vec![]);
        let center = ctx.read_response(canvas_id()).unwrap().rect.center();
        let start = center + egui::vec2(-20., -20.);
        let end = center + egui::vec2(120., 120.);
        frame(
            &mut viewer,
            &ctx,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(&mut viewer, &ctx, vec![egui::Event::PointerMoved(end)]);
        assert!(viewer.current.as_ref().unwrap().selection_box.is_some());
        frame(&mut viewer, &ctx, vec![button(end, false)]);
        assert_eq!(viewer.current.as_ref().unwrap().selected.len(), 3);
        assert!(viewer.current.as_ref().unwrap().selection_box.is_none());
        frame(
            &mut viewer,
            &ctx,
            vec![egui::Event::Key {
                key: Key::G,
                physical_key: Some(Key::G),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(viewer.current.as_ref().unwrap().moving.is_some());
        let placed = end + egui::vec2(40., 60.);
        frame(&mut viewer, &ctx, vec![egui::Event::PointerMoved(placed)]);
        frame(&mut viewer, &ctx, vec![button(placed, true)]);
        frame(&mut viewer, &ctx, vec![button(placed, false)]);
        let view = viewer.current.as_ref().unwrap();
        assert!(view.moving.is_none());
        assert_eq!(
            view.selected.len(),
            3,
            "placement must not replace the group selection"
        );
        assert_eq!(view.loaded.scene.instances[0].at, Point::new(40., 60.));
    }
    #[test]
    fn moving_updates_document_and_scene_and_can_commit_or_cancel() {
        use fschem::{document::Document, geometry::Transform, scene::Shape};
        let paths = LibraryPaths::default();
        let loaded = Arc::new(LoadedDocument::from_document(
            Arc::new(Document::parse("v {xschem file_version=1.3}\nC {test.sym} 0 0 0 0 {name=X1}\n[\nL 4 10 20 30 40 {}\n]\nN 0 0 100 0 {}\n").unwrap()), None, paths.clone(),
        ).unwrap());
        let original = loaded.document.original().to_owned();
        let mut view = DocumentView::new(Arc::clone(&loaded), PaintCache::new(&loaded.scene));
        view.selected = BTreeSet::from([Selection::Instance(0)]);
        // Transform shortcuts are inert until grab mode starts.
        view.update_move(
            Some(Point::new(100., 100.)),
            &[Key::R, Key::X, Key::Y],
            20.,
            &paths,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&view.loaded, &loaded));
        for rotation in 0..4 {
            for flip in [false, true] {
                view.begin_move(Point::new(5., 7.));
                let mut keys = vec![Key::R; rotation];
                if flip {
                    keys.push(Key::Y);
                }
                view.update_move(Some(Point::new(46., 66.)), &keys, 20., &paths)
                    .unwrap();
                let before = view.loaded.scene.instances[0].clone();
                let origin = Point::new(40., 60.);
                assert_eq!(view.moving.as_ref().unwrap().pivot, Point::default());
                assert_eq!(view.moving.as_ref().unwrap().at, origin);
                assert_eq!(before.at, origin);
                for key in [Key::X, Key::Y] {
                    view.update_move(None, &[key], 20., &paths).unwrap();
                    let after = &view.loaded.scene.instances[0];
                    let p = before.at
                        + Transform::new(Point::default(), before.rotation, before.flip)
                            .vector(Point::new(10., 20.))
                        - origin;
                    let expected = origin
                        + if key == Key::X {
                            Point::new(p.x, -p.y)
                        } else {
                            Point::new(-p.x, p.y)
                        };
                    let item = view
                        .loaded
                        .scene
                        .items
                        .iter()
                        .find(|item| item.owner == Some(0))
                        .unwrap();
                    let Shape::Path { points, .. } = &item.shape else {
                        panic!("path expected")
                    };
                    assert_eq!(points[0], expected);
                    assert_eq!(after.at, origin);
                    view.update_move(None, &[key], 20., &paths).unwrap();
                }
                view.cancel_move();
                assert!(Arc::ptr_eq(&view.loaded, &loaded));
                assert_eq!(view.loaded.document.original(), original);
            }
        }
        view.begin_move(Point::default());
        view.update_move(Some(Point::new(40., 60.)), &[Key::R], 20., &paths)
            .unwrap();
        view.commit_move(&paths).unwrap();
        assert!(view.moving.is_none());
        assert_eq!(view.selected_instance(), Some(0));
        assert!(
            view.loaded
                .document
                .original()
                .contains("C {test.sym} 40 60 1 0")
        );
        assert!(view.loaded.document.original().contains("N 0 0 100 0 {}"));
        let committed = Arc::clone(&view.loaded);
        view.begin_move(Point::default());
        view.update_move(Some(Point::new(200., 200.)), &[Key::X], 20., &paths)
            .unwrap();
        view.cancel_move();
        assert!(Arc::ptr_eq(&view.loaded, &committed));
    }

    #[test]
    fn transforms_keep_a_nonzero_origin_fixed_with_an_off_grid_grab() {
        let paths = LibraryPaths::default();
        let mut view = selection_view();
        view.selected = BTreeSet::from([Selection::Instance(0)]);
        view.begin_move(Point::new(3., 7.));
        view.update_move(Some(Point::new(43., 67.)), &[], 20., &paths)
            .unwrap();
        view.commit_move(&paths).unwrap();
        let origin = Point::new(40., 60.);
        let original = view.loaded.document.original().to_owned();
        view.begin_move(Point::new(53., 69.));
        for key in [Key::R, Key::X, Key::Y, Key::R] {
            view.update_move(Some(Point::new(53., 69.)), &[key], 20., &paths)
                .unwrap();
            assert_eq!(view.loaded.scene.instances[0].at, origin);
        }
        view.update_move(Some(Point::new(74., 88.)), &[Key::R], 20., &paths)
            .unwrap();
        assert_eq!(view.loaded.scene.instances[0].at, Point::new(60., 80.));
        let preview = view.loaded.document.original().to_owned();
        view.commit_move(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), preview);
        assert_ne!(preview, original);
    }

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
        parent.selected = BTreeSet::from([Selection::Instance(0)]);
        parent.viewport = Viewport {
            center: Point::new(19., -37.),
            zoom: 2.5,
        };
        parent.fit = false;

        key(&mut viewer, &ctx, Key::E, egui::Modifiers::NONE);
        assert_eq!(viewer.current.as_ref().unwrap().name(), "cell.sch");
        assert_eq!(viewer.navigation_stack.len(), 1);
        let child = viewer.current.as_mut().unwrap();
        child.selected = BTreeSet::from([Selection::Instance(0)]);
        child.layers.insert(4, false);
        key(&mut viewer, &ctx, Key::I, egui::Modifiers::NONE);
        assert_eq!(viewer.current.as_ref().unwrap().name(), "cell.sym");
        assert_eq!(viewer.navigation_stack.len(), 2);

        key(&mut viewer, &ctx, Key::E, egui::Modifiers::SHIFT);
        let child = viewer.current.as_ref().unwrap();
        assert_eq!(child.name(), "cell.sch");
        assert_eq!(child.selected_instance(), Some(0));
        assert!(!child.layers[&4]);
        assert_eq!(viewer.navigation_stack.len(), 1);
        key(&mut viewer, &ctx, Key::E, egui::Modifiers::SHIFT);
        let parent = viewer.current.as_ref().unwrap();
        assert_eq!(parent.name(), "parent.sch");
        assert_eq!(parent.selected_instance(), Some(0));
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
