use crate::canvas::{Canvas, PaintCache, layer_color};
use eframe::egui::{self, Color32, Key, PointerButton, RichText};
use fschem::{
    document::{Document, RecordKind},
    edit::{CommandHistory, DocumentCommand, EditOperation, EditState, Placement},
    geometry::{Bounds, Point, Viewport},
    library::LibraryPaths,
    navigation::{self, Descent, DocumentTarget},
    scene::{LoadedDocument, Selection},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

#[path = "instance_picker.rs"]
mod instance_picker;
use instance_picker::InstancePicker;
#[path = "symbol_tools.rs"]
mod symbol_tools;

const DEMO: &str = include_str!("../examples/demo.sch");

fn create_document(path: &Path) -> Result<(), String> {
    use std::io::Write;
    let kind = match path.extension().and_then(|ext| ext.to_str()) {
        Some("sch") => "",
        Some("sym") => "type=subcircuit",
        _ => return Err("New files must use a .sch or .sym extension".into()),
    };
    let source = format!("v {{xschem version=3.4.6 file_version=1.2}}\nG {{}}\nK {{{kind}}}\nV {{}}\nS {{}}\nE {{}}\n");
    // Never truncate an existing file, even if the dialog permits replacement.
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .open(path).map_err(|error| format!("Cannot create {}: {error}", path.display()))?;
    file.write_all(source.as_bytes())
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))
}
const DEFAULT_GRID_SPACING: f64 = 10.;
const MIN_GRID_SPACING: f64 = 0.001;
const MAX_GRID_SPACING: f64 = 1e6;

fn canvas_id() -> egui::Id {
    egui::Id::new("schematic_canvas")
}

fn format_grid_spacing(spacing: f64) -> String {
    let mut text = format!("{spacing:.6}");
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    text
}

pub struct Options {
    pub input: Option<PathBuf>,
    pub open_as_text: bool,
    pub open_run: bool,
    pub libraries: LibraryPaths,
    pub smoke_test: bool,
    pub screenshot: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}
impl FileStamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}
fn session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
struct DocumentView {
    identity: u64,
    record_ids: Vec<u64>,
    identity_history: Vec<(Arc<Document>, Vec<u64>)>,
    loaded: Arc<LoadedDocument>,
    saved: Arc<Document>,
    file_stamp: Option<FileStamp>,
    cache: PaintCache,
    viewport: Viewport,
    layers: BTreeMap<u16, bool>,
    selected: BTreeSet<Selection>,
    selection_box: Option<Point>,
    fit: bool,
    target: Option<DocumentTarget>,
    moving: Option<MoveState>,
    wiring: Option<WireState>,
    symbol_tools: symbol_tools::SymbolTools,
    history: CommandHistory,
    graphs: crate::schematic_graphs::SchematicGraphs,
}
struct MoveState {
    original: Arc<LoadedDocument>,
    restore: Arc<LoadedDocument>,
    selection_before: BTreeSet<Selection>,
    copying: bool,
    inserting: bool,
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
    horizontal_first: bool,
}
impl WireState {
    fn bend(&self) -> Point {
        if self.horizontal_first {
            Point::new(self.end.x, self.start.y)
        } else {
            Point::new(self.start.x, self.end.y)
        }
    }
}
impl DocumentView {
    fn new(loaded: Arc<LoadedDocument>, cache: PaintCache) -> Self {
        let target = loaded.path.clone().map(DocumentTarget::File);
        let file_stamp = loaded.path.as_deref().and_then(FileStamp::read);
        Self {
            identity: session_id(),
            record_ids: loaded.document.records.iter().map(|_| session_id()).collect(),
            identity_history: Vec::new(),
            saved: Arc::clone(&loaded.document),
            file_stamp,
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
            symbol_tools: Default::default(),
            history: CommandHistory::default(),
            graphs: Default::default(),
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
        self.file_stamp = loaded.path.as_deref().and_then(FileStamp::read);
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
            inserting: false,
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
        if let Some((_, ids)) = self.identity_history.iter().find(|(doc, _)| Arc::ptr_eq(doc, &state.document)) {
            self.record_ids = ids.clone();
        } else {
            self.record_ids.resize_with(state.document.records.len(), session_id);
            self.identity_history.push((state.document.clone(), self.record_ids.clone()));
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
        let old_ids = self.record_ids.clone();
        self.identity_history.push((before.document.clone(), old_ids.clone()));
        if matches!(&operation, EditOperation::Delete) {
            self.record_ids = if before.document.records.iter().any(|record| matches!(record.kind, RecordKind::Embedded(_))) {
                // Deleting an embedded owner can relocate definition records. Never guess indices.
                Vec::new()
            } else {
                old_ids.iter().enumerate().filter_map(|(i, id)| (!before.selection.contains(&i)).then_some(*id)).collect()
            };
        }
        let mut history = std::mem::take(&mut self.history);
        let result = history.execute(Box::new(DocumentCommand::new(operation, before)), |state| {
            self.apply_edit_state(state, paths)
        });
        self.history = history;
        if result.is_err() { self.record_ids = old_ids; }
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
    fn is_symbol(&self) -> bool {
        match &self.target {
            Some(DocumentTarget::File(path)) => path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("sym")),
            Some(DocumentTarget::Embedded { .. }) => true,
            None => false,
        }
    }
    fn begin_symbol_primitive(&mut self, source: &str, paths: &LibraryPaths) -> Result<(), String> {
        if !self.is_symbol() || self.moving.is_some() || self.wiring.is_some() {
            return Err("Symbol drawing tools require an idle symbol document".into());
        }
        let (document, index) = self.loaded.document.with_symbol_primitive(source)?;
        let restore = self.loaded.clone();
        let selection_before = self.selected.clone();
        self.install_document(document, paths)?;
        self.select_records(&BTreeSet::from([index]));
        self.begin_move(Point::default());
        if let Some(moving) = &mut self.moving {
            moving.restore = restore;
            moving.selection_before = selection_before;
            moving.inserting = true;
        }
        Ok(())
    }
    fn begin_instance(&mut self, path: &Path, paths: &LibraryPaths) -> Result<(), String> {
        let symbol = fschem::library::read_document(path)?;
        let template = symbol.symbol_properties().and_then(|p| p.get("template")).unwrap_or("");
        let (document, index) = self.loaded.document.with_instance(&path.to_string_lossy(), template)?;
        let restore = self.loaded.clone();
        let selection_before = self.selected.clone();
        self.install_document(document, paths)?;
        self.select_records(&BTreeSet::from([index]));
        self.begin_move(Point::default());
        if let Some(moving) = &mut self.moving {
            moving.restore = restore;
            moving.selection_before = selection_before;
            moving.inserting = true;
        }
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
            horizontal_first: true,
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
        let bend = wiring.bend();
        let horizontal_first = wiring.horizontal_first;
        if start != end {
            self.execute_command(EditOperation::WireRoute { start, bend, end }, self.edit_state(), paths)?;
        }
        self.wiring = if on_connection {
            None
        } else {
            Some(WireState {
                start: end,
                end,
                on_connection: false,
                horizontal_first,
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
            self.layers.retain(|layer, _| self.loaded.scene.layers.contains_key(layer));
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
        let operation = if moving.inserting {
            if moving.records.iter().any(|&index| matches!(moving.original.document.records[index].kind, RecordKind::Instance { .. })) {
                EditOperation::Insert(self.edit_state())
            } else {
                EditOperation::InsertPrimitive(self.edit_state())
            }
        } else if moving.copying {
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
    fn selected_instance_records(&self) -> BTreeSet<usize> {
        self.selected
            .iter()
            .filter_map(|selection| match selection {
                Selection::Instance(index) => {
                    self.loaded.scene.instance_records.get(index).copied()
                }
                _ => None,
            })
            .collect()
    }
    fn graph_properties(&self, index: usize) -> Option<&fschem::document::Properties> {
        match &self.loaded.document.records.get(index)?.kind {
            RecordKind::Rectangle { properties, .. } if properties.get("flags").is_some_and(|f| f.split_whitespace().any(|f| f == "graph")) => Some(properties),
            _ => None,
        }
    }
    fn graph_at(&self, pointer: Option<Point>) -> Option<usize> {
        self.selected_graph_records().first().copied().or_else(|| {
            let p = pointer?;
            self.loaded.document.records.iter().enumerate().rev().find_map(|(index, record)| {
                if self.graph_properties(index).is_none() { return None; }
                let RecordKind::Rectangle { a, b, layer, .. } = &record.kind else { return None; };
                (self.layers.get(layer) != Some(&false) && p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)).then_some(index)
            })
        })
    }
    fn selected_graph_records(&self) -> BTreeSet<usize> {
        self.selected.iter().filter_map(|selection| {
            let Selection::Primitive(index) = selection else { return None; };
            match &self.loaded.document.records.get(*index)?.kind {
                RecordKind::Rectangle { properties, .. }
                    if properties.get("flags").is_some_and(|flags| flags.split_whitespace().any(|flag| flag == "graph")) => Some(*index),
                _ => None,
            }
        }).collect()
    }
    fn set_selected_graph_property(
        &mut self,
        key: String,
        value: String,
        paths: &LibraryPaths,
    ) -> Result<(), String> {
        let records = self.selected_graph_records();
        if records.is_empty() { return Ok(()); }
        let before = EditState {
            document: Arc::clone(&self.loaded.document),
            selection: records,
        };
        // Only graph records are edited, even when components are also selected.
        self.execute_command(EditOperation::SetRecordProperty { key, value }, before, paths)
    }
    fn set_selected_instance_property(
        &mut self,
        key: String,
        value: String,
        paths: &LibraryPaths,
    ) -> Result<(), String> {
        let records = self.selected_instance_records();
        if records.is_empty() {
            return Ok(());
        }
        let before = EditState {
            document: Arc::clone(&self.loaded.document),
            selection: records,
        };
        self.execute_command(
            EditOperation::SetInstanceProperty { key, value },
            before,
            paths,
        )
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
            if let Some((id, _)) = hits.first()
                && !self.selected.remove(id) {
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
#[derive(Default)]
struct DocumentTab {
    text: Option<crate::text_viewer::TextDocument>,
    current: Option<DocumentView>,
    stack: Vec<DocumentView>,
}

fn tab_label(stack: &[DocumentView], current: Option<&DocumentView>) -> String {
    tab_names(stack, current).join(">")
}

fn tab_names(stack: &[DocumentView], current: Option<&DocumentView>) -> Vec<String> {
    stack
        .iter()
        .chain(current)
        .map(|view| {
            let name = view.name();
            let stem = Path::new(&name)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            format!("{stem}{}", if view.modified() { "*" } else { "" })
        })
        .collect()
}

/// One flat hit target and background, with a separate, unboxed close target.
fn document_tab(ui: &mut egui::Ui, names: &[String], active: bool) -> (bool, bool) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let foreground = if active {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().text_color()
    };
    let muted = foreground.gamma_multiply(0.65);
    let names = if names.is_empty() {
        &["Start".to_owned()][..]
    } else {
        names
    };
    let labels: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            ui.painter().layout_no_wrap(
                name.clone(),
                font.clone(),
                if index + 1 == names.len() {
                    foreground
                } else {
                    muted
                },
            )
        })
        .collect();
    let height = (font.size + 10.).max(26.);
    let separator_width = 24.;
    let close_width = 26.;
    let width = 10.
        + labels.iter().map(|label| label.size().x).sum::<f32>()
        + separator_width * (labels.len() - 1) as f32
        + 6.
        + close_width;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let close_rect =
        egui::Rect::from_min_max(egui::pos2(rect.right() - close_width, rect.top()), rect.max);
    let body_rect =
        egui::Rect::from_min_max(rect.min, egui::pos2(close_rect.left(), rect.bottom()));
    let body = ui.interact(body_rect, response.id.with("body"), egui::Sense::click());
    let close = ui
        .interact(close_rect, response.id.with("close"), egui::Sense::click())
        .on_hover_text("Close tab (Cmd/Ctrl-W)");
    let fill = if active {
        ui.visuals().selection.bg_fill
    } else if body.hovered() || close.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().widgets.inactive.weak_bg_fill
    };
    ui.painter().rect_filled(rect, 0., fill);
    let mut x = rect.left() + 10.;
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            let center = egui::pos2(x + separator_width / 2., rect.center().y);
            ui.painter().add(egui::Shape::line(
                vec![
                    center + egui::vec2(-2., -4.),
                    center + egui::vec2(2., 0.),
                    center + egui::vec2(-2., 4.),
                ],
                egui::Stroke::new(1.4, muted),
            ));
            x += separator_width;
        }
        ui.painter().galley(
            egui::pos2(x, rect.center().y - label.size().y / 2.),
            label.clone(),
            foreground,
        );
        x += label.size().x;
    }
    let center = close_rect.center();
    let stroke = egui::Stroke::new(1.3, if close.hovered() { foreground } else { muted });
    for sign in [-1., 1.] {
        ui.painter().line_segment(
            [
                center + egui::vec2(-3., -3. * sign),
                center + egui::vec2(3., 3. * sign),
            ],
            stroke,
        );
    }
    (body.clicked(), close.clicked())
}

// Cancelled Save As and failed writes also cancel the destructive operation.
fn confirm_leave(view: &mut DocumentView, paths: &LibraryPaths) -> Result<bool, String> {
    view.cancel_move();
    view.wiring = None;
    view.selection_box = None;
    if !view.modified() {
        return Ok(true);
    }
    let answer = rfd::MessageDialog::new()
        .set_title("Unsaved changes")
        .set_description(format!("Save changes to {} before leaving?", view.name()))
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
            "Save".into(),
            "Don't Save".into(),
            "Cancel".into(),
        ))
        .show();
    match answer {
        rfd::MessageDialogResult::Yes => {}
        rfd::MessageDialogResult::No => return Ok(true),
        rfd::MessageDialogResult::Custom(ref label) if label == "Save" => {}
        rfd::MessageDialogResult::Custom(ref label) if label == "Don't Save" => return Ok(true),
        _ => return Ok(false),
    }
    let path = view.loaded.path.clone().or_else(|| {
        rfd::FileDialog::new()
            .set_title("Save schematic or symbol")
            .add_filter("Xschem schematic or symbol", &["sch", "sym"])
            .set_file_name(view.name())
            .save_file()
            .map(|mut path| {
                if path.extension().is_none() {
                    path.set_extension(
                        Path::new(&view.name())
                            .extension()
                            .unwrap_or(std::ffi::OsStr::new("sch")),
                    );
                }
                path
            })
    });
    let Some(path) = path else {
        return Ok(false);
    };
    view.save_to(&path, paths)?;
    Ok(true)
}

enum Action {
    Undo,
    Redo,
    Edit(Key),
    Open(PathBuf),
    Save,
    SaveAs,
    New,
    OpenDialog,
    OpenTextDialog,
    OpenText(PathBuf),
    OpenRun(PathBuf),
    OpenRunDialog,
    CloseTab,
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
    run_view: Option<crate::run_artifact::RunArtifact>,
    #[cfg(target_os = "macos")]
    native_menu: Option<crate::macos_menu::NativeMenu>,
    text: Option<crate::text_viewer::TextDocument>,
    current: Option<DocumentView>,
    /// Active tab state; inactive tabs are parked in `tabs`.
    navigation_stack: Vec<DocumentView>,
    tabs: Vec<DocumentTab>,
    active_tab: usize,
    opening_tab: bool,
    paths: LibraryPaths,
    sender: mpsc::Sender<Completion>,
    receiver: mpsc::Receiver<Completion>,
    request: u64,
    loading: Option<PathBuf>,
    error: Option<String>,
    export: Option<mpsc::Receiver<Result<ExportCompletion, String>>>,
    export_status: Option<String>,
    simulator: fschem::simulation::Simulator,
    waveforms: crate::waveforms::Waveforms,
    show_grid: bool,
    grid_spacing: f64,
    show_hidden: bool,
    show_libraries: bool,
    instance_picker: Option<InstancePicker>,
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
    last_file_check: Instant,
}
impl Viewer {
    pub(crate) fn set_simulator(&mut self, simulator: fschem::simulation::Simulator) {
        self.simulator = simulator;
    }
    pub fn new(ctx: &egui::Context, options: Options, smoke_success: Arc<AtomicBool>) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        let (sender, receiver) = mpsc::channel();
        let mut viewer = Self {
            run_view: None,
            #[cfg(target_os = "macos")]
            native_menu: None,
            text: None,
            current: None,
            navigation_stack: Vec::new(),
            tabs: vec![DocumentTab::default()],
            active_tab: 0,
            opening_tab: false,
            paths: options.libraries,
            sender,
            receiver,
            request: 0,
            loading: None,
            error: None,
            export: None,
            export_status: None,
            simulator: fschem::simulation::Simulator::default(),
            waveforms: crate::waveforms::Waveforms::default(),
            show_grid: true,
            grid_spacing: DEFAULT_GRID_SPACING,
            show_hidden: false,
            show_libraries: false,
            instance_picker: None,
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
            last_file_check: Instant::now(),
        };
        if let Some(path) = options.input {
            viewer.action(ctx, if options.open_run { Action::OpenRun(path) } else if options.open_as_text { Action::OpenText(path) } else { Action::Open(path) });
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
            menu.sync_simulator(self.simulator);
            menu.sync_history(
                if keyboard { self.undo_label() } else { None },
                if keyboard { self.redo_label() } else { None },
            );
            menu.sync(
                self.current.is_some(),
                self.text.is_some(),
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

    fn park_tab(&mut self) {
        self.tabs[self.active_tab] = DocumentTab {
            text: self.text.take(),
            current: self.current.take(),
            stack: std::mem::take(&mut self.navigation_stack),
        };
    }

    fn switch_tab(&mut self, index: usize) {
        if index == self.active_tab {
            return;
        }
        // Invalidate pending loads so they cannot land in another tab.
        self.request += 1;
        self.loading = None;
        self.opening_tab = false;
        self.park_tab();
        self.active_tab = index;
        let tab = std::mem::take(&mut self.tabs[index]);
        self.text = tab.text;
        self.current = tab.current;
        self.navigation_stack = tab.stack;
        self.pointer = None;
        self.last_pointer = None;
        self.error = None;
    }

    fn open_tab(&mut self, ctx: &egui::Context, path: Option<PathBuf>) {
        self.load(ctx, path, false);
        self.opening_tab = true;
    }

    fn confirm_active(&mut self, whole_stack: bool) -> bool {
        let views = self.current.iter_mut().chain(
            self.navigation_stack
                .iter_mut()
                .rev()
                .take(if whole_stack { usize::MAX } else { 0 }),
        );
        for view in views {
            match confirm_leave(view, &self.paths) {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) => {
                    self.error = Some(error);
                    return false;
                }
            }
        }
        true
    }

    fn close_tab(&mut self, index: usize) {
        self.switch_tab(index);
        if !self.confirm_active(true) {
            return;
        }
        self.request += 1;
        self.loading = None;
        self.current = None;
        self.text = None;
        self.navigation_stack.clear();
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(DocumentTab::default());
        }
        self.active_tab = index.min(self.tabs.len() - 1);
        let tab = std::mem::take(&mut self.tabs[self.active_tab]);
        self.text = tab.text;
        self.current = tab.current;
        self.navigation_stack = tab.stack;
    }

    fn confirm_quit(&mut self) -> bool {
        let original = self.active_tab;
        for index in 0..self.tabs.len() {
            self.switch_tab(index);
            if !self.confirm_active(true) {
                return false;
            }
        }
        self.switch_tab(original);
        true
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
        self.opening_tab = false;
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
                    if self.opening_tab && (self.current.is_some() || self.text.is_some()) {
                        self.park_tab();
                        self.tabs.push(DocumentTab::default());
                        self.active_tab = self.tabs.len() - 1;
                    }
                    self.opening_tab = false;
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
        self.check_auto_reload(ctx);
    }
    fn check_auto_reload(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(Duration::from_secs(1));
        if self.loading.is_some() || self.last_file_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_file_check = Instant::now();
        let Some(current) = &mut self.current else {
            return;
        };
        let Some(path) = current.loaded.path.clone() else {
            return;
        };
        let Some(stamp) = FileStamp::read(&path) else {
            return;
        };
        if current.file_stamp.is_none() {
            current.file_stamp = Some(stamp);
            return;
        }
        if current.file_stamp == Some(stamp) {
            return;
        }
        if current.modified() {
            current.file_stamp = Some(stamp);
            self.error = Some(format!(
                "{} changed on disk, but this view has unsaved edits. Use Reload to discard local edits or Save to overwrite the file.",
                path.display()
            ));
            return;
        }
        self.load_target(ctx, Some(DocumentTarget::File(path)), false);
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
            Key::R => view.moving.is_some() || view.wiring.is_some(),
            Key::X | Key::Y => view.moving.is_some(),
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
        if self.text.is_some() {
            if matches!(action, Action::Reload) {
                self.text.as_mut().unwrap().reload(ctx);
                return;
            }
            if !matches!(action, Action::New | Action::OpenRun(_) | Action::OpenRunDialog | Action::Open(_) | Action::OpenText(_) | Action::OpenTextDialog | Action::OpenDialog | Action::CloseTab | Action::Demo | Action::OpenRaw) {
                return;
            }
        }
        if (matches!(action, Action::Back) && !self.navigation_stack.is_empty()
            || matches!(action, Action::Reload))
            && !self.confirm_active(false)
        {
            return;
        }
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
                Key::R if current.wiring.is_some() => {
                    let wiring = current.wiring.as_mut().unwrap();
                    wiring.horizontal_first = !wiring.horizontal_first;
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
            Action::OpenRun(path) => {
                match crate::run_artifact::RunArtifact::open(&path) {
                    Ok(run) => { self.run_view = Some(run); self.error = None; }
                    Err(error) => self.error = Some(error),
                }
            }
            Action::OpenRunDialog => {
                if let Some(path) = rfd::FileDialog::new().set_title("Open run manifest").add_filter("Run manifest", &["json"]).pick_file() { self.action(ctx, Action::OpenRun(path)); }
            }
            Action::OpenText(path) => {
                self.request += 1;
                self.loading = None;
                self.opening_tab = false;
                if self.current.is_some() || self.text.is_some() {
                    self.park_tab();
                    self.tabs.push(DocumentTab::default());
                    self.active_tab = self.tabs.len() - 1;
                }
                self.text = Some(crate::text_viewer::TextDocument::new(ctx, path));
            }
            Action::OpenTextDialog => {
                if let Some(path) = rfd::FileDialog::new().set_title("Open as read-only text").pick_file() {
                    self.action(ctx, Action::OpenText(path));
                }
            }
            Action::Open(path) => {
                if path.is_dir() || path.file_name().is_some_and(|n| n == "run.json") {
                    self.action(ctx, Action::OpenRun(path));
                    return;
                }
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("raw"))
                {
                    self.waveforms.load(ctx, path);
                } else if crate::text_viewer::is_text(&path) {
                    self.action(ctx, Action::OpenText(path));
                } else {
                    self.open_tab(ctx, Some(path));
                }
            }
            Action::CloseTab => self.close_tab(self.active_tab),
            Action::OpenRaw => self.waveforms.open_dialog(ctx),
            Action::OpenDialog => {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("Schematic or text", &["sch", "sym", "py", "spice", "sim", "log", "lib", "va", "json", "toml", "txt", "raw"]);
                if let Some(dir) = self
                    .current
                    .as_ref()
                    .and_then(|v| v.target.as_ref())
                    .map(DocumentTarget::directory)
                {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    self.action(ctx, Action::Open(path));
                }
            }
            Action::New => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Create schematic or symbol")
                    .set_file_name("untitled.sch")
                    .add_filter("Xschem schematic", &["sch"])
                    .add_filter("Xschem symbol", &["sym"])
                    .save_file()
                {
                    let path = if path.extension().is_none() { path.with_extension("sch") } else { path };
                    match create_document(&path) {
                        Ok(()) => self.open_tab(ctx, Some(path)),
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            Action::Demo => self.open_tab(ctx, None),
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
                        .set_title(format!("Run {} — save raw results", self.simulator.name()))
                        .add_filter("Simulation raw data", &["raw"])
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
                    let simulator = self.simulator;
                    self.export_status = Some(if simulate {
                        format!("Generating netlist and running {}…", simulator.name())
                    } else {
                        "Exporting SPICE netlist…".into()
                    });
                    self.error = None;
                    std::thread::spawn(move || {
                        let result = if simulate {
                            fschem::simulation::run_with_simulator(simulator, &document, &source, paths, &path).map(|files| {
                                let mut message = format!(
                                    "Simulation complete: {} · Netlist: {} · Log: {}",
                                    files.raw.display(),
                                    files.netlist.display(),
                                    files.log.display()
                                );
                                if !files.warnings.is_empty() {
                                    message.push_str(&format!(
                                        "\n{} simulator startup notice(s); see log. {}",
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
                if matches!(kind, Descent::Schematic) && self.loading.is_none() {
                    if let Some(current) = &self.current {
                        if current.moving.is_none() && let Some(index) = current.graph_at(self.pointer) {
                            let properties = current.graph_properties(index).unwrap().clone();
                            if let Some(path) = crate::schematic_graphs::raw_path(current.loaded.path.as_deref(), &properties) {
                                self.waveforms.open_graph(ctx, crate::waveforms::GraphTarget { document: current.identity, record: current.record_ids[index] }, path, properties);
                            } else { self.error = Some("Save the schematic or set graph rawfile first.".into()); }
                            return;
                        }
                    }
                }
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
    fn sync_linked_graph(&mut self, ctx: &egui::Context) {
        let views = self.current.iter_mut().chain(self.navigation_stack.iter_mut())
            .chain(self.tabs.iter_mut().flat_map(|tab| tab.current.iter_mut().chain(tab.stack.iter_mut())));
        let mut views: Vec<_> = views.collect();
        let paths = &self.paths;
        let error = &mut self.error;
        self.waveforms.sync_graph(ctx, |target, previous, edits| {
            let view = views.iter_mut().find(|view| view.identity == target.document)?;
            let index = view.record_ids.iter().position(|id| *id == target.record)?;
            let properties = view.graph_properties(index)?;
            // A schematic edit/undo wins over a queued viewport edit from an older snapshot.
            if properties.raw == previous.raw && !edits.is_empty() && view.moving.is_none() {
                let before = view.edit_state();
                if let Err(message) = view.execute_command(EditOperation::SetGraphConfiguration { records: BTreeSet::from([index]), properties: edits }, before, paths) { *error = Some(message); }
            }
            let properties = view.graph_properties(index)?.clone();
            let path = crate::schematic_graphs::raw_path(view.loaded.path.as_deref(), &properties)?;
            Some((path, properties))
        });
    }
    fn show(&mut self, ctx: &egui::Context) {
        self.sync_linked_graph(ctx);
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
        let mut property_edits: Vec<(String, String)> = Vec::new();
        let mut graph_property_edits: Vec<(String, String)> = Vec::new();
        #[cfg(target_os = "macos")]
        if let Some(menu) = &self.native_menu {
            use crate::macos_menu::Command;
            for command in menu.commands() {
                match command {
                    Command::New => actions.push(Action::New),
                    Command::Open => actions.push(Action::OpenDialog),
                    Command::OpenText => actions.push(Action::OpenTextDialog),
                    Command::CloseTab => actions.push(Action::CloseTab),
                    Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Command::Undo => actions.push(Action::Undo),
                    Command::Redo => actions.push(Action::Redo),
                    Command::Edit(key) => actions.push(Action::Edit(key)),
                    Command::Save => actions.push(Action::Save),
                    Command::SaveAs => actions.push(Action::SaveAs),
                    Command::Demo => actions.push(Action::Demo),
                    Command::Reload => actions.push(Action::Reload),
                    Command::ExportSpice => actions.push(Action::ExportSpice),
                    Command::Simulate => actions.push(Action::Simulate),
                    Command::Simulator(simulator) => self.simulator = simulator,
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
                Key::N,
            )) {
                actions.push(Action::New);
            }
            if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                Key::O,
            )) {
                actions.push(Action::OpenDialog);
            }
            if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                Key::W,
            )) {
                actions.push(Action::CloseTab);
            }
            if input.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                Key::R,
            )) {
                actions.push(Action::Reload);
            }
        });
        if self.instance_picker.is_none() && (!ctx.wants_keyboard_input() || ctx.memory(|memory| memory.has_focus(canvas_id()))) {
            if ctx.input(|i| i.modifiers == egui::Modifiers::SHIFT && i.key_pressed(Key::I))
                && self.current.as_ref().is_some_and(|v| v.moving.is_none() && v.wiring.is_none())
            {
                self.instance_picker = Some(InstancePicker::new(&self.paths));
            }
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
                    if ui.button("Open run…").clicked() { actions.push(Action::OpenRunDialog); }
                    if ui.button("Open as text…").clicked() { actions.push(Action::OpenTextDialog); ui.close(); }
                    if ui.button("New…     Ctrl/Command-N").clicked() {
                        actions.push(Action::New);
                        ui.close();
                    }
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
                    if ui.button("Close tab     Ctrl/Command-W").clicked() {
                        actions.push(Action::CloseTab);
                        ui.close();
                    }
                    if ui.button("Built-in demonstration").clicked() {
                        actions.push(Action::Demo);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.current.is_some() || self.text.is_some(),
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
                ui.menu_button("Simulator", |ui| {
                    for simulator in fschem::simulation::Simulator::ALL {
                        if ui.selectable_value(&mut self.simulator, simulator, simulator.name()).clicked() {
                            ui.close();
                        }
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
                if ui.button("Open run…").clicked() { actions.push(Action::OpenRunDialog); }
                    if ui.button("Open as text…").clicked() { actions.push(Action::OpenTextDialog); }
                if ui.button("New…").clicked() {
                    actions.push(Action::New);
                }
                if ui.button("Open…").clicked() {
                    actions.push(Action::OpenDialog);
                }
                if ui.add_enabled(self.can_save(), egui::Button::new("Save")).on_hover_text("Save schematic or symbol (Cmd/Ctrl-S)").clicked() {
                    actions.push(Action::Save);
                }
                if ui.button("Open raw…").on_hover_text("Plot simulation raw data in a separate waveform window").clicked() {
                    actions.push(Action::OpenRaw);
                }
                egui::ComboBox::from_id_salt("run-simulator")
                    .selected_text(self.simulator.name())
                    .width(95.)
                    .show_ui(ui, |ui| {
                        for simulator in fschem::simulation::Simulator::ALL {
                            ui.selectable_value(&mut self.simulator, simulator, simulator.name());
                        }
                    });
                if ui.add_enabled(self.can_export(), egui::Button::new("▶"))
                    .on_hover_text(format!("Run {} — generate a netlist and save raw results and log", self.simulator.name()))
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
                ui.label(format!("Spacing {}", format_grid_spacing(self.grid_spacing)));
                if ui
                    .add_enabled(
                        self.grid_spacing > MIN_GRID_SPACING,
                        egui::Button::new("½ grid"),
                    )
                    .clicked()
                {
                    self.grid_spacing = (self.grid_spacing / 2.).max(MIN_GRID_SPACING);
                }
                if ui
                    .add_enabled(
                        self.grid_spacing < MAX_GRID_SPACING,
                        egui::Button::new("2× grid"),
                    )
                    .clicked()
                {
                    self.grid_spacing = (self.grid_spacing * 2.).min(MAX_GRID_SPACING);
                }
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
        let mut switch = None;
        let mut close = None;
        egui::TopBottomPanel::top("document_tabs")
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, tab) in self.tabs.iter().enumerate() {
                            let active = index == self.active_tab;
                            let text = if active { self.text.as_ref() } else { tab.text.as_ref() };
                            let names = if let Some(text) = text { vec![text.name()] } else if active {
                                tab_names(&self.navigation_stack, self.current.as_ref())
                            } else {
                                tab_names(&tab.stack, tab.current.as_ref())
                            };
                            let (selected, closed) = document_tab(ui, &names, active);
                            if selected {
                                switch = Some(index);
                            }
                            if closed {
                                close = Some(index);
                            }
                        }
                    });
                });
            });
        if let Some(index) = switch {
            self.switch_tab(index);
        }
        if let Some(index) = close {
            self.close_tab(index);
        }
        let title = self.text.as_ref().map(|t| t.name()).unwrap_or_else(|| tab_label(&self.navigation_stack, self.current.as_ref()));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(if title.is_empty() {
            "fschem".into()
        } else {
            format!("{title} — fschem")
        }));
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
                } else if let Some(text) = &self.text {
                    ui.label(format!("Read-only text · {}", text.path.display()));
                } else {
                    ui.label("Open a schematic or text file, or drag one into this window");
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
                    if current.is_symbol() {
                        let idle = current.moving.is_none() && current.wiring.is_none() && self.loading.is_none();
                        let primitive = ui.add_enabled_ui(idle, |ui| current.symbol_tools.show(ui)).inner;
                        if let Some(source) = primitive {
                            if let Err(error) = current.begin_symbol_primitive(&source, &self.paths) {
                                self.error = Some(error);
                            }
                        }
                        ui.separator();
                    }
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
                                ui.checkbox(visible, format!("{layer}: {} ({})", layer_name(layer), loaded.scene.layers.get(&layer).copied().unwrap_or(0)));
                            });
                        }
                    });
                    ui.separator();
                    ui.heading("Selection");
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
                    let selected_instances: Vec<_> = current
                        .selected
                        .iter()
                        .filter_map(|selection| match selection {
                            Selection::Instance(index) => loaded.scene.instances.get(*index),
                            _ => None,
                        })
                        .collect();
                    if !selected_instances.is_empty() {
                        if selected_instances.len() == 1 {
                            let instance = selected_instances[0];
                            ui.strong(instance.properties.get("name").unwrap_or("Unnamed instance"));
                            ui.label(&instance.symbol);
                            ui.monospace(format!("({}, {}) · rot {} · flip {}", instance.at.x, instance.at.y, instance.rotation, instance.flip));
                            ui.horizontal(|ui| {
                                if ui.add_enabled(self.loading.is_none(), egui::Button::new("Symbol (i)")).clicked() { actions.push(Action::Descend(Descent::Symbol)); }
                                if ui.add_enabled(self.loading.is_none(), egui::Button::new("Schematic (e)")).clicked() { actions.push(Action::Descend(Descent::Schematic)); }
                            });
                            if instance.embedded { ui.weak("Definition embedded in this document"); }
                            else if instance.resolved.is_none() { ui.colored_label(Color32::LIGHT_RED, "Unresolved symbol — see diagnostics"); }
                        } else {
                            ui.strong(format!("{} instances", selected_instances.len()));
                            ui.weak("Edit a value to apply it to every selected instance.");
                        }
                        ui.separator();
                        ui.heading("Properties");
                        let mut rows: BTreeMap<String, (Option<String>, bool, usize)> = BTreeMap::new();
                        for instance in &selected_instances {
                            let mut seen = BTreeSet::new();
                            for (key, value) in &instance.properties.entries {
                                if !seen.insert(key.clone()) {
                                    continue;
                                }
                                let value = value.clone().unwrap_or_default();
                                rows.entry(key.clone())
                                    .and_modify(|(first, mixed, count)| {
                                        if first.as_deref() != Some(value.as_str()) {
                                            *mixed = true;
                                        }
                                        *count += 1;
                                    })
                                    .or_insert((Some(value), false, 1));
                            }
                        }
                        egui::Grid::new("instance_property_editor")
                            .num_columns(2)
                            .striped(true)
                            .show(ui, |ui| {
                                for (key, (value, mut mixed, count)) in rows {
                                    if count != selected_instances.len() {
                                        mixed = true;
                                    }
                                    ui.monospace(&key);
                                    let mut text = if mixed { "*".to_owned() } else { value.unwrap_or_default() };
                                    let large = !mixed && is_large_property_value(&text);
                                    let rows = property_editor_rows(&text);
                                    let response = if large {
                                        ui.add(
                                            egui::TextEdit::multiline(&mut text)
                                                .desired_width(f32::INFINITY)
                                                .desired_rows(rows),
                                        )
                                    } else {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut text)
                                                .desired_width(f32::INFINITY),
                                        )
                                    };
                                    if response.changed() {
                                        property_edits.push((key, text));
                                    }
                                    ui.end_row();
                                }
                            });
                    }
                    let graph_records = current.selected_graph_records();
                    let graphs: Vec<_> = graph_records.iter().filter_map(|&index| {
                        match &loaded.document.records[index].kind {
                            RecordKind::Rectangle { properties, .. } => Some(properties),
                            _ => None,
                        }
                    }).collect();
                    if !graphs.is_empty() {
                        ui.separator();
                        graph_properties_ui(ui, &graphs, loaded.path.as_deref(), &mut graph_property_edits);
                    } else if selected_instances.is_empty() {
                        ui.weak("Click a component, graph or wire to select it. Left-drag to box select. Hold Shift to add to the selection. G: move · C: copy · W: wire · i: symbol · e: schematic · Shift-E: up.");
                    }
                } else if let Some(text) = &self.text {
                    ui.label(text.path.display().to_string());
                    ui.weak("Read-only text. Files are never executed or modified. Use the viewer controls to search, reload or follow changes.");
                } else { ui.weak("No document open"); }
            });
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                if let Some(text) = &mut self.text {
                    text.ui(ui);
                } else if let Some(current) = &mut self.current {
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
                    };
                    self.pointer = response.hover_pos().map(|pos| canvas.world(pos));
                    if self.pointer.is_some() { self.last_pointer = self.pointer; }
                    let keyboard = self.instance_picker.is_none() && (!ctx.wants_keyboard_input() || response.has_focus());
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
                        if keys.contains(&Key::R) {
                            let wiring = current.wiring.as_mut().unwrap();
                            wiring.horizontal_first = !wiring.horizontal_first;
                        }
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
                    current.graphs.paint(ui, &canvas, &current.loaded, &current.layers);
                    if let Some(wiring) = &current.wiring {
                        let bend = wiring.bend();
                        if wiring.start != bend {
                            canvas.wire_preview(wiring.start, bend, wiring.on_connection && bend == wiring.end);
                        }
                        if bend != wiring.end {
                            canvas.wire_preview(bend, wiring.end, wiring.on_connection);
                        }
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
                        if ui.button("New schematic or symbol…").clicked() {
                            actions.push(Action::New);
                        }
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
        if let Some(current) = &mut self.current {
            for (key, value) in graph_property_edits {
                if let Err(error) = current.set_selected_graph_property(key, value, &self.paths) {
                    self.error = Some(error);
                }
            }
            for (key, value) in property_edits {
                if let Err(error) = current.set_selected_instance_property(key, value, &self.paths)
                {
                    self.error = Some(error);
                }
            }
        }
        for action in actions {
            self.action(ctx, action);
        }
        #[cfg(target_os = "macos")]
        self.sync_native_menu(ctx);
        self.automation(ctx);
    }
    fn windows(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if let Some(picker) = &mut self.instance_picker {
            let (closed, chosen) = picker.show(ctx);
            if closed || chosen.is_some() { self.instance_picker = None; }
            if let Some(path) = chosen {
                ctx.memory_mut(|m| m.request_focus(canvas_id()));
                if let Some(view) = &mut self.current {
                    if let Err(error) = view.begin_instance(&path, &self.paths) { self.error = Some(error); }
                }
            }
        }
        let mut keep_run = true;
        if let Some(run) = &self.run_view {
            egui::Window::new("Run artifacts (read only)").open(&mut keep_run).default_size([650., 500.]).show(ctx, |ui| {
                ui.label(run.path.display().to_string());
                ui.heading(format!("{} · {} · {}", run.manifest.status, run.manifest.backend, run.manifest.mode));
                ui.label("Opening artifacts never executes code. Reload to observe lifecycle changes.");
                if ui.button("Reload manifest").clicked() { actions.push(Action::OpenRun(run.path.clone())); }
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(run.manifest.source.display().to_string());
                        if ui.add_enabled(run.manifest.source.is_file(), egui::Button::new("Open source")).clicked() { actions.push(Action::Open(run.manifest.source.clone())); }
                    });
                    ui.collapsing("Analysis specification", |ui| { ui.monospace(serde_json::to_string_pretty(&run.manifest.analyses).unwrap_or_default()); });
                    ui.heading("Results");
                    if run.manifest.results.is_empty() { ui.label("No published results (prepared/partial/failed runs retain text artifacts)."); }
                    for result in &run.manifest.results {
                        let path = crate::run_artifact::contained(&run.root, &result.path);
                        ui.horizontal(|ui| {
                            ui.label(format!("{} — {} — {}", result.id, result.analysis_name, result.name));
                            match path {
                                Ok(path) if path.is_file() => { if ui.button("Graphs").clicked() { self.waveforms.load_plot(ctx, path, result.plot_index); } }
                                _ => { ui.colored_label(Color32::LIGHT_RED, "Missing or unsafe raw file"); }
                            }
                        });
                    }
                    ui.heading("Text artifacts / measurements");
                    for (label, path) in &run.files {
                        let safe = path.strip_prefix(&run.root).ok().and_then(|p| crate::run_artifact::contained(&run.root, &p.to_string_lossy()).ok()).is_some();
                        if ui.add_enabled(safe && path.is_file(), egui::Button::new(format!("{label}: {}", path.display()))).clicked() { actions.push(Action::OpenText(path.clone())); }
                        if !safe || !path.is_file() { ui.label("Missing/unavailable (normal for logs before execution)"); }
                    }
                    if !run.files.iter().any(|(label, _)| label == "measurements") { ui.label("No measurement artifact published; native measurements may appear in the log."); }
                    for diagnostic in &run.manifest.diagnostics { ui.monospace(diagnostic.to_string()); }
                });
            });
        }
        if !keep_run { self.run_view = None; }

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
                    if let Some(path) = &current.loaded.path
                        && ui.button("Open disk source in text tab").clicked() { actions.push(Action::OpenText(path.clone())); }
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
        if let Some(text) = &self.text {
            if text.failed() && self.smoke { ctx.send_viewport_cmd(egui::ViewportCommand::Close); return; }
            if !text.ready() { return; }
        } else if self.current.is_none() && self.run_view.is_none() { return; }
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
        if ctx.input(|input| input.viewport().close_requested()) && !self.confirm_quit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
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

fn graph_properties_ui(
    ui: &mut egui::Ui,
    graphs: &[&fschem::document::Properties],
    document: Option<&Path>,
    edits: &mut Vec<(String, String)>,
) {
    ui.heading("Graph properties");
    if graphs.len() > 1 {
        ui.weak(format!("{} graphs · edits apply to all selected graphs", graphs.len()));
    }
    ui.weak("Raw file: absolute path or relative to the schematic. Leave empty to use the schematic's .raw file.");
    let mut keys: BTreeSet<String> = ["rawfile", "node", "dataset", "x1", "x2", "y1", "y2", "logx", "groupmode", "mode", "fschem_expressions"]
        .into_iter().map(str::to_owned).collect();
    for props in graphs {
        keys.extend(props.entries.iter().map(|(key, _)| key.clone()));
    }
    // Keep the source selector first, then show every stored graph property.
    keys.remove("rawfile");
    egui::Grid::new("graph_property_editor").num_columns(2).striped(true).show(ui, |ui| {
        for key in std::iter::once("rawfile".to_owned()).chain(keys) {
            let first = graphs[0].get(&key).unwrap_or_default();
            let mixed = graphs.iter().any(|props| props.get(&key).unwrap_or_default() != first);
            let mut text = if mixed { "*".to_owned() } else { first.to_owned() };
            ui.monospace(&key);
            ui.vertical(|ui| {
                let response = if key == "node" || is_large_property_value(&text) {
                    let rows = property_editor_rows(&text).max(2);
                    ui.add(egui::TextEdit::multiline(&mut text).desired_width(f32::INFINITY).desired_rows(rows))
                } else {
                    ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY)
                        .hint_text(if key == "rawfile" { "Automatic (.raw beside schematic)" } else { "Automatic" }))
                };
                if response.changed() { edits.push((key.clone(), text)); }
                if key == "rawfile" && ui.button("Browse raw file…").clicked() {
                    let mut dialog = rfd::FileDialog::new().set_title("Graph source raw file")
                        .add_filter("Simulation raw data", &["raw"]);
                    if let Some(directory) = document.and_then(Path::parent) {
                        dialog = dialog.set_directory(directory);
                    }
                    if let Some(path) = dialog.pick_file() {
                        edits.push((key.clone(), path.to_string_lossy().into_owned()));
                    }
                }
            });
            ui.end_row();
        }
    });
}

fn is_large_property_value(text: &str) -> bool {
    text.contains('\n') || text.chars().count() > 80
}

fn property_editor_rows(text: &str) -> usize {
    let line_count = text.lines().count().max(1);
    let wrapped_rows = (text.chars().count() / 80).max(1);
    line_count.max(wrapped_rows).clamp(3, 16)
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_documents_are_valid_and_never_overwrite_files() {
        let dir = std::env::temp_dir().join(format!("fschem-new-{}-{}", std::process::id(), super::session_id()));
        std::fs::create_dir_all(&dir).unwrap();
        for extension in ["sch", "sym"] {
            let path = dir.join(format!("new.{extension}"));
            super::create_document(&path).unwrap();
            let source = std::fs::read_to_string(&path).unwrap();
            let document = fschem::document::Document::parse(source.as_str()).unwrap();
            assert!(document.diagnostics.is_empty());
            assert!(super::create_document(&path).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        }
        assert!(super::create_document(&dir.join("invalid.txt")).is_err());
        assert!(!dir.join("invalid.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;
    #[test]
    fn graph_properties_edit_source_round_trip_and_undo() {
        let source = "v {xschem file_version=1.3}\nB 2 0 0 400 200 {flags=graph node=out custom=retained}\nB 2 0 250 400 450 {flags=graph node=in}\nN 0 0 100 0 {lab=signal}\n";
        let paths = LibraryPaths::default();
        let loaded = Arc::new(LoadedDocument::from_document(
            Arc::new(Document::parse(source).unwrap()), None, paths.clone(),
        ).unwrap());
        let mut view = DocumentView::new(Arc::clone(&loaded), PaintCache::new(&loaded.scene));
        view.selected = BTreeSet::from([Selection::Primitive(1), Selection::Primitive(2)]);
        assert_eq!(view.selected_graph_records(), BTreeSet::from([1, 2]));
        let path = "results with spaces/run.raw";
        view.set_selected_graph_property("rawfile".into(), path.into(), &paths).unwrap();
        let edited = view.loaded.document.original().to_owned();
        let reparsed = Document::parse(edited.as_str()).unwrap();
        for index in [1, 2] {
            let RecordKind::Rectangle { properties, .. } = &reparsed.records[index].kind else { panic!() };
            assert_eq!(properties.get("rawfile"), Some(path));
            assert_eq!(properties.get("flags"), Some("graph"));
        }
        assert_eq!(reparsed.raw_record(&reparsed.records[3]), loaded.document.raw_record(&loaded.document.records[3]));
        view.undo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), source);
        view.redo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), edited);
        view.set_selected_graph_property("node".into(), "out\nin".into(), &paths).unwrap();
        let RecordKind::Rectangle { properties, .. } = &view.loaded.document.records[1].kind else { panic!() };
        assert_eq!(properties.get("node"), Some("out\nin"));
        assert_eq!(properties.get("custom"), Some("retained"));
    }

    #[test]
    fn graph_property_panel_exposes_source_and_stored_properties() {
        let props = fschem::document::Properties::parse("flags=graph node=out custom=retained".into());
        let ctx = egui::Context::default();
        let mut edits = Vec::new();
        let mut render = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                graph_properties_ui(ui, &[&props], Some(Path::new("test.sch")), &mut edits);
            });
        };
        let _ = ctx.run(egui::RawInput::default(), &mut render);
        let output = ctx.run(egui::RawInput::default(), &mut render);
        let labels: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        }).collect();
        assert!(labels.contains(&"rawfile"));
        assert!(labels.contains(&"Browse raw file…"));
        assert!(labels.contains(&"node"));
        assert!(labels.contains(&"custom"));
        assert!(edits.is_empty());
    }

    fn linked_graph_view() -> DocumentView {
        let document = Document::parse("v {xschem file_version=1.3}\nN 0 0 10 0 {}\nB 2 0 0 400 200 {flags=graph rawfile=missing.raw node=out custom=retained}\nB 2 0 250 400 450 {flags=graph node=in}\n").unwrap();
        let loaded = Arc::new(LoadedDocument::from_document(Arc::new(document), None, LibraryPaths::default()).unwrap());
        DocumentView::new(loaded.clone(), PaintCache::new(&loaded.scene))
    }

    #[test]
    fn linked_graph_identity_survives_shift_copy_and_history() {
        let paths = LibraryPaths::default();
        let mut view = linked_graph_view();
        let id = view.record_ids[2];
        assert_eq!(view.graph_at(Some(Point::new(20., 300.))), Some(3));
        view.selected = BTreeSet::from([Selection::Primitive(2)]);
        assert_eq!(view.graph_at(Some(Point::new(20., 300.))), Some(2));
        view.selected = BTreeSet::from([Selection::Wire(1)]);
        view.delete_selection(&paths).unwrap();
        assert_eq!(view.record_ids[1], id);
        view.undo(&paths).unwrap();
        assert_eq!(view.record_ids[2], id);
        view.redo(&paths).unwrap();
        assert_eq!(view.record_ids[1], id);
        let before = EditState { document: view.loaded.document.clone(), selection: BTreeSet::from([1]) };
        view.execute_command(EditOperation::Copy(Placement { pivot: Point::default(), at: Point::default(), rotation: 0, flip: false }), before, &paths).unwrap();
        assert_eq!(view.record_ids[1], id);
        assert_ne!(*view.record_ids.last().unwrap(), id);
        view.undo(&paths).unwrap();
        view.redo(&paths).unwrap();
        assert_eq!(view.record_ids[1], id);
    }

    #[test]
    fn linked_graph_updates_parked_origin_undo_and_detaches_on_replacement() {
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(&ctx, Options { input: None, open_as_text: false, open_run: false, libraries: LibraryPaths::default(), smoke_test: false, screenshot: None }, Arc::new(AtomicBool::new(false)));
        viewer.current = Some(linked_graph_view());
        viewer.pointer = Some(Point::new(20., 20.));
        viewer.action(&ctx, Action::Descend(Descent::Schematic));
        let target = viewer.waveforms.test_link(Some(("groupmode", "separate"))).unwrap();
        viewer.current.as_mut().unwrap().selected = BTreeSet::from([Selection::Wire(1)]);
        let origin = viewer.current.take();
        viewer.tabs.push(DocumentTab { current: origin, ..Default::default() });
        viewer.current = Some(linked_graph_view());
        viewer.sync_linked_graph(&ctx);
        assert!(!viewer.current.as_ref().unwrap().modified());
        let origin = viewer.tabs.last_mut().unwrap().current.as_mut().unwrap();
        assert_eq!(origin.identity, target.document);
        assert!(origin.modified());
        assert_eq!(origin.graph_properties(2).unwrap().get("groupmode"), Some("separate"));
        assert_eq!(origin.graph_properties(2).unwrap().get("custom"), Some("retained"));
        assert_eq!(origin.history.undo_label(), Some("Edit graph"));
        let selection = BTreeSet::from([Selection::Wire(1)]);
        assert_eq!(origin.selected, selection);
        origin.undo(&viewer.paths).unwrap();
        assert_eq!(origin.selected, selection);
        origin.redo(&viewer.paths).unwrap();
        assert_eq!(origin.selected, selection);
        origin.undo(&viewer.paths).unwrap();
        viewer.waveforms.test_link(Some(("node", "stale")));
        viewer.sync_linked_graph(&ctx);
        let origin = viewer.tabs.last().unwrap().current.as_ref().unwrap();
        assert!(!origin.modified(), "undo wins over queued stale viewport changes");
        // Hierarchy parking also retains the same owner.
        viewer.navigation_stack.push(viewer.tabs.last_mut().unwrap().current.take().unwrap());
        viewer.waveforms.test_link(Some(("node", "in")));
        viewer.sync_linked_graph(&ctx);
        assert_eq!(viewer.navigation_stack[0].graph_properties(2).unwrap().get("node"), Some("in"));
        // Reload/close replace or remove the session owner, never retarget by path/index.
        viewer.navigation_stack.clear();
        viewer.waveforms.test_link(Some(("node", "wrong")));
        viewer.sync_linked_graph(&ctx);
        assert!(viewer.waveforms.test_link(None).is_none());
        assert!(!viewer.current.as_ref().unwrap().modified());
    }

    #[test]
    fn linked_graph_rejects_edits_during_movement_without_history() {
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(&ctx, Options { input: None, open_as_text: false, open_run: false, libraries: LibraryPaths::default(), smoke_test: false, screenshot: None }, Arc::new(AtomicBool::new(false)));
        viewer.current = Some(linked_graph_view());
        viewer.pointer = Some(Point::new(20., 20.));
        viewer.action(&ctx, Action::Descend(Descent::Schematic));
        let view = viewer.current.as_mut().unwrap();
        view.selected = BTreeSet::from([Selection::Wire(1)]);
        view.begin_move(Point::default());
        assert!(view.moving.is_some());
        viewer.waveforms.test_link(Some(("groupmode", "separate")));
        viewer.sync_linked_graph(&ctx);
        let view = viewer.current.as_mut().unwrap();
        assert_eq!(view.graph_properties(2).unwrap().get("groupmode"), None);
        assert!(view.history.undo_label().is_none());
        view.commit_move(&viewer.paths).unwrap();
        viewer.sync_linked_graph(&ctx);
        assert_eq!(viewer.current.as_ref().unwrap().graph_properties(2).unwrap().get("groupmode"), None);
    }

    #[test]
    fn deleting_linked_graph_detaches_without_retargeting_neighbor() {
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(&ctx, Options { input: None, open_as_text: false, open_run: false, libraries: LibraryPaths::default(), smoke_test: false, screenshot: None }, Arc::new(AtomicBool::new(false)));
        viewer.current = Some(linked_graph_view());
        viewer.pointer = Some(Point::new(20., 20.));
        viewer.action(&ctx, Action::Descend(Descent::Schematic));
        viewer.waveforms.test_link(Some(("node", "wrong")));
        let view = viewer.current.as_mut().unwrap();
        view.selected = BTreeSet::from([Selection::Primitive(2)]);
        view.delete_selection(&viewer.paths).unwrap();
        viewer.sync_linked_graph(&ctx);
        assert!(viewer.waveforms.test_link(None).is_none());
        assert_eq!(viewer.current.as_ref().unwrap().graph_properties(2).unwrap().get("node"), Some("in"));
        viewer.current.as_mut().unwrap().undo(&viewer.paths).unwrap();
        viewer.sync_linked_graph(&ctx);
        assert!(viewer.waveforms.test_link(None).is_none(), "deletion detaches until explicit E");
    }

    #[test]
    fn library_instance_placement_cancels_and_undoes() {
        let paths = LibraryPaths::default();
        let path = std::env::temp_dir().join(format!("fschem-picker-{}.sym", session_id()));
        std::fs::write(&path, "v {xschem file_version=1.3}\nK {template=\"name=R1 value=1k\"}\nL 17 -10 0 10 0 {}\n").unwrap();
        let mut view = selection_view();
        let original = view.loaded.document.original().to_owned();
        let original_layers = view.layers.clone();
        view.begin_instance(&path, &paths).unwrap();
        assert!(view.layers.contains_key(&17));
        view.cancel_move();
        assert!(view.moving.is_none());
        assert_eq!(view.layers, original_layers);
        assert_eq!(view.loaded.document.original(), original);
        view.begin_instance(&path, &paths).unwrap();
        view.update_move(Some(Point::new(40., 60.)), &[], 10., &paths).unwrap();
        view.commit_move(&paths).unwrap();
        let placed = view.loaded.document.original().to_owned();
        assert!(placed.contains("40 60 0 0"));
        assert!(view.loaded.document.records.iter().any(|r| matches!(&r.kind,
            RecordKind::Instance { properties, .. } if properties.get("value") == Some("1k"))));
        assert_eq!(view.history.undo_label(), Some("Place instance"));
        view.undo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), original);
        view.redo(&paths).unwrap();
        assert_eq!(view.loaded.document.original(), placed);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn symbol_tools_place_cancel_and_undo_all_primitive_types() {
        let paths = LibraryPaths::default();
        let loaded = Arc::new(LoadedDocument::from_document(
            Arc::new(Document::parse("v {xschem file_version=1.2}\nK {type=subcircuit}\n").unwrap()),
            None, paths.clone(),
        ).unwrap());
        let mut view = DocumentView::new(loaded.clone(), PaintCache::new(&loaded.scene));
        assert!(!view.is_symbol());
        assert!(view.begin_symbol_primitive("L 4 0 0 40 0 {}\n", &paths).is_err());
        view.target = Some(DocumentTarget::File(PathBuf::from("test.sym")));
        assert!(view.is_symbol());
        for primitive in [
            "L 4 0 0 40 0 {}\n", "B 4 0 0 40 40 {}\n",
            "A 4 0 0 20 0 360 {}\n", "T {@name} 0 0 0 0 0.3 0.3 {layer=4}\n",
            "B 5 -2.5 -2.5 2.5 2.5 {name=P1 dir=inout}\n",
        ] {
            let before = view.loaded.document.original().to_owned();
            view.begin_symbol_primitive(primitive, &paths).unwrap();
            assert!(view.moving.is_some());
            view.cancel_move();
            assert_eq!(view.loaded.document.original(), before);
            view.begin_symbol_primitive(primitive, &paths).unwrap();
            view.update_move(Some(Point::new(40., 60.)), &[Key::R], 10., &paths).unwrap();
            view.commit_move(&paths).unwrap();
            assert_eq!(view.history.undo_label(), Some("Place symbol primitive"));
            let placed = view.loaded.document.original().to_owned();
            assert_ne!(placed, before);
            view.undo(&paths).unwrap();
            assert_eq!(view.loaded.document.original(), before);
            view.redo(&paths).unwrap();
            assert_eq!(view.loaded.document.original(), placed);
        }
        assert!(view.begin_symbol_primitive("B 5 0 0 5 5 {name=P1 dir=in}\n", &paths).is_err());
        view.target = Some(DocumentTarget::Embedded {
            document: view.loaded.document.clone(), source: PathBuf::from("parent.sch"), symbol: "cell.sym".into(),
        });
        assert!(view.is_symbol());
        view.target = Some(DocumentTarget::File(PathBuf::from("test.sch")));
        assert!(!view.is_symbol());
    }

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
                open_as_text: false, open_run: false,
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
        // Return to the saved state before leaving; edited children now prompt.
        shortcut(&mut viewer, &ctx, false);
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
                open_as_text: false, open_run: false,
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
                open_as_text: false, open_run: false,
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
        key(&mut viewer, &ctx, Key::R);
        assert!(!viewer.current.as_ref().unwrap().wiring.as_ref().unwrap().horizontal_first);
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
                (Point::new(40., 0.), Point::new(40., 93.)),
                (Point::new(40., 93.), Point::new(187., 93.))
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
    fn shift_clicks_toggle_items_and_shift_boxes_add_items() {
        let mut view = selection_view();
        view.select_at(Point::new(-8., -8.), false, false, false);
        assert_eq!(view.selected, BTreeSet::from([Selection::Instance(0)]));
        view.select_at(Point::new(60., 0.), false, false, true);
        assert_eq!(
            view.selected,
            BTreeSet::from([Selection::Instance(0), Selection::Wire(3)])
        );
        let selection = view.selected.clone();
        view.select_at(Point::new(50., 50.), false, false, true);
        assert!(view.selected.contains(&Selection::Wire(5)));
        assert_eq!(view.selected.len(), 3);
        view.select_at(Point::new(50., 50.), false, false, true);
        assert_eq!(view.selected, selection, "Shift-click deselects the hit item even with overlaps");
        view.select_at(Point::new(300., 300.), false, false, true);
        assert_eq!(view.selected, selection, "empty space preserves the group");
        view.select_at(Point::new(-8., -8.), false, false, true);
        assert_eq!(view.selected, BTreeSet::from([Selection::Wire(3)]));
        view.select_at(Point::new(-8., -8.), false, false, true);
        assert_eq!(view.selected, selection);
        view.select_box(
            Bounds::new(Point::new(190., 190.), Point::new(210., 210.)),
            false,
            true,
        );
        assert_eq!(view.selected.len(), 3);
        view.select_box(
            Bounds::new(Point::new(300., 300.), Point::new(310., 310.)),
            false,
            true,
        );
        assert_eq!(view.selected.len(), 3);
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
                open_as_text: false, open_run: false,
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
    fn text_tabs_isolate_actions_and_preserve_dirty_schematic_hierarchy() {
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(&ctx, Options { input: None, open_as_text: false, open_run: false, libraries: LibraryPaths::default(), smoke_test: false, screenshot: None }, Arc::new(AtomicBool::new(false)));
        let mut view = selection_view();
        view.selected = BTreeSet::from([Selection::Instance(0)]);
        view.delete_selection(&viewer.paths).unwrap();
        let original = view.loaded.document.original().to_owned();
        viewer.current = Some(view);
        viewer.navigation_stack.push(selection_view());
        viewer.action(&ctx, Action::OpenText(PathBuf::from("Cargo.toml")));
        assert!(viewer.current.is_none());
        assert!(viewer.navigation_stack.is_empty());
        assert!(!viewer.can_save()); assert!(!viewer.can_export());
        assert!(viewer.undo_label().is_none());
        for action in [Action::Undo, Action::Redo, Action::Edit(Key::Backspace), Action::Save, Action::Back, Action::Descend(Descent::Symbol)] { viewer.action(&ctx, action); }
        assert!(viewer.text.is_some());
        viewer.switch_tab(0);
        assert_eq!(viewer.current.as_ref().unwrap().loaded.document.original(), original);
        assert!(viewer.current.as_ref().unwrap().modified());
        assert_eq!(viewer.navigation_stack.len(), 1);
        assert!(viewer.undo_label().is_some());
        viewer.switch_tab(1);
        viewer.close_tab(1);
        assert_eq!(viewer.tabs.len(), 1);
        assert!(viewer.current.as_ref().unwrap().modified());
    }

    #[test]
    fn tabs_preserve_unsaved_documents_and_undo_history() {
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(
            &ctx,
            Options {
                input: None,
                open_as_text: false, open_run: false,
                libraries: LibraryPaths::default(),
                smoke_test: false,
                screenshot: None,
            },
            Arc::new(AtomicBool::new(false)),
        );
        let mut view = selection_view();
        view.selected = BTreeSet::from([Selection::Instance(0)]);
        view.delete_selection(&viewer.paths).unwrap();
        assert!(view.modified());
        viewer.current = Some(view);
        viewer.navigation_stack.push(selection_view());
        let label = tab_label(&viewer.navigation_stack, viewer.current.as_ref());
        assert!(label.ends_with('*'));
        viewer.tabs.push(DocumentTab {
            text: None,
            current: Some(selection_view()),
            stack: Vec::new(),
        });
        viewer.switch_tab(1);
        assert!(!viewer.current.as_ref().unwrap().modified());
        assert!(viewer.navigation_stack.is_empty());
        viewer.switch_tab(0);
        assert_eq!(
            tab_label(&viewer.navigation_stack, viewer.current.as_ref()),
            label
        );
        assert!(viewer.current.as_ref().unwrap().modified());
        viewer.action(&ctx, Action::Undo);
        assert!(!viewer.current.as_ref().unwrap().modified());
        viewer.close_tab(0);
        viewer.close_tab(0);
        assert!(viewer.current.is_none());
        assert!(viewer.navigation_stack.is_empty());
        assert_eq!(viewer.tabs.len(), 1);
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
                open_as_text: false, open_run: false,
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
        assert_eq!(
            tab_label(&viewer.navigation_stack, viewer.current.as_ref()),
            "parent>cell>cell"
        );

        // Opening is not a descent: a fresh tab starts at depth zero.
        viewer.action(&ctx, Action::Open(dir.join("cell.sch")));
        settle(&mut viewer, &ctx);
        assert_eq!(viewer.tabs.len(), 2);
        assert_eq!(viewer.active_tab, 1);
        assert!(viewer.navigation_stack.is_empty());
        assert_eq!(
            tab_label(&viewer.navigation_stack, viewer.current.as_ref()),
            "cell"
        );
        viewer.switch_tab(0);
        assert_eq!(viewer.navigation_stack.len(), 2);
        assert_eq!(viewer.current.as_ref().unwrap().name(), "cell.sym");
        viewer.switch_tab(1);
        viewer.close_tab(1);
        assert_eq!(viewer.tabs.len(), 1);
        assert_eq!(viewer.navigation_stack.len(), 2);

        // Failed opens leave the current tab and its stack untouched.
        viewer.action(&ctx, Action::Open(dir.join("missing.sch")));
        let deadline = Instant::now() + Duration::from_secs(2);
        while viewer.loading.is_some() {
            viewer.poll(&ctx);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(viewer.error.take().is_some());
        assert_eq!(viewer.tabs.len(), 1);
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
                open_as_text: false, open_run: false,
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
