use crate::{
    document::{Document, Properties, RecordKind},
    geometry::{Bounds, Point, Transform},
    library::{Library, LibraryPaths, read_document},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Fill {
    #[default]
    None,
    Stipple,
    Solid,
}

#[derive(Clone, Debug)]
pub struct Style {
    pub width: f64,
    pub bus: bool,
    pub dash: f64,
    pub fill: Fill,
}
impl Style {
    fn new(p: &Properties, rectangle: bool) -> Self {
        let width = p.number("bus").unwrap_or(0.).max(0.);
        let fill = match p.get("fill") {
            Some("full" | "2") => Fill::Solid,
            Some("true" | "1" | "yes") => Fill::Stipple,
            None if rectangle => Fill::Stipple,
            _ => Fill::None,
        };
        Self {
            width,
            bus: p.flag("bus") || p.number("bus") == Some(-1.),
            dash: p.number("dash").unwrap_or(0.).max(0.),
            fill,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextRun {
    pub text: String,
    pub anchor: Point,
    pub rotation: u8,
    pub flip: bool,
    pub scale: Point,
    pub bounds: Bounds,
}
impl TextRun {
    pub fn new(
        text: String,
        anchor: Point,
        rotation: u8,
        flip: bool,
        scale: Point,
        hcenter: bool,
        vcenter: bool,
    ) -> Self {
        let width = text
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0) as f64
            * 25.5
            * scale.x;
        let height = (text.bytes().filter(|b| *b == b'\n').count() + 1) as f64 * 57.2 * scale.y;
        let left = if hcenter { -width / 2. } else { 0. };
        let top = if vcenter { -height / 2. } else { 0. };
        let local = Bounds::new(
            Point::new(left, top),
            Point::new(left + width, top + height),
        );
        let bounds = Transform::new(anchor, rotation, flip).bounds(local);
        Self {
            text,
            anchor,
            rotation,
            flip,
            scale,
            bounds,
        }
    }
    /// Text stays readable under 180° rotation/mirroring, as in Xschem.
    pub fn glyph_transform(&self) -> Transform {
        if self.rotation % 2 == 1 {
            Transform::new(Point::new(self.bounds.min.x, self.bounds.max.y), 3, false)
        } else {
            Transform::new(self.bounds.min, 0, false)
        }
    }
}

#[derive(Clone, Debug)]
pub enum Shape {
    Path { points: Vec<Point>, closed: bool },
    Text(TextRun),
}
#[derive(Clone, Debug)]
pub struct Item {
    pub shape: Shape,
    pub bounds: Bounds,
    pub layer: u16,
    pub style: Style,
    pub hidden: bool,
    pub owner: Option<usize>,
    pub wire_record: Option<usize>,
    pub primitive_record: Option<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Selection {
    Instance(usize),
    Wire(usize),
    Primitive(usize),
}
impl Item {
    pub fn selection(&self) -> Option<Selection> {
        self.owner
            .map(Selection::Instance)
            .or_else(|| self.wire_record.map(Selection::Wire))
            .or_else(|| self.primitive_record.map(Selection::Primitive))
    }
}
#[derive(Clone, Debug)]
pub struct InstanceInfo {
    pub symbol: String,
    pub resolved: Option<PathBuf>,
    pub embedded: bool,
    /// Document containing this instance, including for composed symbols.
    pub source_file: PathBuf,
    /// Keep the actual definition so navigation also works for embedded symbols.
    pub definition: Option<Arc<Document>>,
    pub at: Point,
    pub rotation: u8,
    pub flip: bool,
    pub properties: Properties,
    pub bounds: Bounds,
}

#[derive(Clone, Debug)]
pub struct SceneDiagnostic {
    pub file: PathBuf,
    pub line: usize,
    pub message: String,
}

pub struct Scene {
    /// Electrical pin centers in world coordinates, for interactive wire snapping.
    pub ports: Vec<Point>,
    /// Scene instance index to editable top-level document record index.
    pub instance_records: BTreeMap<usize, usize>,
    pub items: Vec<Item>,
    pub instances: Vec<InstanceInfo>,
    pub bounds: Bounds,
    pub layers: BTreeMap<u16, usize>,
    pub diagnostics: Vec<SceneDiagnostic>,
    pub dependencies: Vec<PathBuf>,
}
pub struct LoadedDocument {
    pub document: Arc<Document>,
    pub path: Option<PathBuf>,
    pub scene: Scene,
}
impl LoadedDocument {
    pub fn open(path: &Path, paths: LibraryPaths) -> Result<Self, String> {
        let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let document = read_document(&path)?;
        Self::from_document(document, Some(path), paths)
    }
    pub fn from_document(
        document: Arc<Document>,
        path: Option<PathBuf>,
        paths: LibraryPaths,
    ) -> Result<Self, String> {
        let source_path = path.clone().unwrap_or_else(|| PathBuf::from("demo.sch"));
        Self::from_document_at(document, path, source_path, paths)
    }
    pub fn from_document_at(
        document: Arc<Document>,
        path: Option<PathBuf>,
        source_path: PathBuf,
        paths: LibraryPaths,
    ) -> Result<Self, String> {
        let mut builder = Builder::new(paths);
        builder.visit(&document, &source_path, Transform::default(), None, None, 0)?;
        builder.junctions()?;
        if builder.dynamic_labels > 0 {
            builder.warn(&source_path, 0, format!("{} label(s) contain unresolved attributes/Tcl expressions; original text is preserved", builder.dynamic_labels));
        }
        builder.items.sort_by_key(|item| item.layer);
        let mut layers = BTreeMap::new();
        let mut bounds: Option<Bounds> = None;
        for item in &builder.items {
            *layers.entry(item.layer).or_default() += 1;
            if !item.hidden {
                if let Some(bounds) = &mut bounds {
                    bounds.include(item.bounds);
                } else {
                    bounds = Some(item.bounds);
                }
            }
        }
        let scene = Scene {
            ports: builder.ports,
            instance_records: builder.instance_records,
            items: builder.items,
            instances: builder.instances,
            bounds: bounds.unwrap_or(Bounds::new(
                Point::new(-100., -100.),
                Point::new(100., 100.),
            )),
            layers,
            diagnostics: builder.diagnostics,
            dependencies: builder.dependencies,
        };
        Ok(Self {
            document,
            path,
            scene,
        })
    }
}

struct LabelContext<'a> {
    instance: &'a Properties,
    template: Properties,
    symbol: &'a str,
    pins: Vec<&'a Properties>,
    rotation: u8,
    flip: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SymbolKey {
    File(PathBuf),
    Embedded(PathBuf, String),
}

struct Builder {
    ports: Vec<Point>,
    instance_records: BTreeMap<usize, usize>,
    library: Library,
    items: Vec<Item>,
    instances: Vec<InstanceInfo>,
    diagnostics: Vec<SceneDiagnostic>,
    dependencies: Vec<PathBuf>,
    active: HashSet<SymbolKey>,
    reported: HashSet<(PathBuf, usize, String)>,
    wires: Vec<(Point, Point)>,
    dynamic_labels: usize,
}
impl Builder {
    fn new(paths: LibraryPaths) -> Self {
        Self {
            instance_records: BTreeMap::new(),
            ports: Vec::new(),
            library: Library::new(paths),
            items: Vec::new(),
            instances: Vec::new(),
            diagnostics: Vec::new(),
            dependencies: Vec::new(),
            active: HashSet::new(),
            reported: HashSet::new(),
            wires: Vec::new(),
            dynamic_labels: 0,
        }
    }
    fn warn(&mut self, file: &Path, line: usize, message: impl Into<String>) {
        let message = message.into();
        if self
            .reported
            .insert((file.to_path_buf(), line, message.clone()))
        {
            self.diagnostics.push(SceneDiagnostic {
                file: file.to_path_buf(),
                line,
                message,
            });
        }
    }
    fn path(
        &mut self,
        points: Vec<Point>,
        closed: bool,
        layer: u16,
        style: Style,
        owner: Option<usize>,
    ) {
        if let Some(bounds) = Bounds::from_points(points.iter().copied()) {
            self.items.push(Item {
                shape: Shape::Path { points, closed },
                bounds,
                layer,
                style,
                hidden: false,
                owner,
                wire_record: None,
                primitive_record: None,
            });
        }
    }
    fn visit(
        &mut self,
        doc: &Document,
        file: &Path,
        transform: Transform,
        context: Option<&LabelContext<'_>>,
        owner: Option<usize>,
        depth: usize,
    ) -> Result<(), String> {
        if self.items.len() > 1_000_000 {
            return Err("scene exceeds one million drawing items".into());
        }
        for diagnostic in &doc.diagnostics {
            self.warn(file, diagnostic.line, &diagnostic.message);
        }
        if !matches!(doc.version(), "1.0" | "1.1" | "1.2" | "1.3") {
            self.warn(
                file,
                1,
                format!(
                    "unrecognized file version {}; reading known records conservatively",
                    doc.version()
                ),
            );
        }
        let mut embedded = HashMap::new();
        let mut last_instance = None;
        for record in &doc.records {
            match &record.kind {
                RecordKind::Instance { symbol, .. } => last_instance = Some(symbol.as_str()),
                RecordKind::Embedded(records) => {
                    if let Some(symbol) = last_instance {
                        embedded
                            .entry(symbol)
                            .or_insert_with(|| Arc::new(doc.embedded(records)));
                    }
                }
                _ => {}
            }
        }
        for (record_index, record) in doc.records.iter().enumerate() {
            let first_item = self.items.len();
            if self.items.len() >= 1_000_000 {
                return Err("scene exceeds one million drawing items".into());
            }
            match &record.kind {
                RecordKind::Line {
                    layer,
                    a,
                    b,
                    properties,
                } => self.path(
                    vec![transform.point(*a), transform.point(*b)],
                    false,
                    *layer,
                    Style::new(properties, false),
                    owner,
                ),
                RecordKind::Wire { a, b, properties } => {
                    let (a, b) = (transform.point(*a), transform.point(*b));
                    self.wires.push((a, b));
                    self.path(vec![a, b], false, 1, Style::new(properties, false), owner);
                    if depth == 0
                        && let Some(item) = self.items.last_mut()
                    {
                        item.wire_record = Some(record_index);
                    }
                }
                RecordKind::Rectangle {
                    layer,
                    a,
                    b,
                    properties,
                } => {
                    if *layer == 5 && depth <= 1 {
                        self.ports.push(transform.point((*a + *b) * 0.5));
                    }
                    let bounds = Bounds::new(*a, *b);
                    let mut style = Style::new(properties, true);
                    if *layer == 2
                        && properties
                            .get("flags")
                            .is_some_and(|v| v.contains("graph") || v.contains("image"))
                    {
                        if properties.get("flags").is_some_and(|v| v.contains("image")) {
                            self.warn(
                                file,
                                record.line,
                                "image rectangle preserved; displaying its boundary only",
                            );
                        }
                        style.fill = Fill::None;
                    }
                    let mut closed = true;
                    let points = if let Some(ellipse) = properties.get("ellipse") {
                        let values: Vec<_> = ellipse
                            .split([',', ' '])
                            .filter_map(|s| s.parse::<f64>().ok())
                            .collect();
                        closed = values.get(1).copied().unwrap_or(360.).abs() >= 360.
                            || style.fill != Fill::None;
                        arc_points(
                            bounds.center(),
                            bounds.size() * 0.5,
                            values.first().copied().unwrap_or(0.),
                            values.get(1).copied().unwrap_or(360.),
                            style.fill != Fill::None,
                        )
                    } else {
                        vec![
                            bounds.min,
                            Point::new(bounds.max.x, bounds.min.y),
                            bounds.max,
                            Point::new(bounds.min.x, bounds.max.y),
                        ]
                    };
                    self.path(
                        points.into_iter().map(|p| transform.point(p)).collect(),
                        closed,
                        *layer,
                        style,
                        owner,
                    );
                }
                RecordKind::Arc {
                    layer,
                    center,
                    radius,
                    start,
                    sweep,
                    properties,
                } => {
                    let style = Style::new(properties, false);
                    let filled = style.fill != Fill::None;
                    self.path(
                        arc_points(
                            *center,
                            Point::new(radius.abs(), radius.abs()),
                            *start,
                            *sweep,
                            filled,
                        )
                        .into_iter()
                        .map(|p| transform.point(p))
                        .collect(),
                        filled || sweep.abs() >= 360.,
                        *layer,
                        style,
                        owner,
                    );
                }
                RecordKind::Polygon {
                    layer,
                    points,
                    properties,
                } => {
                    let closed = points.first() == points.last();
                    let mut style = Style::new(properties, false);
                    if !closed {
                        style.fill = Fill::None;
                    }
                    let points = if properties.flag("bezier") {
                        bezier_points(points)
                    } else {
                        points.clone()
                    };
                    self.path(
                        points.into_iter().map(|p| transform.point(p)).collect(),
                        closed,
                        *layer,
                        style,
                        owner,
                    );
                }
                RecordKind::Text {
                    text,
                    at,
                    rotation,
                    flip,
                    scale,
                    properties,
                } => {
                    let (text, unresolved) = expand_label(text, context);
                    if unresolved {
                        self.dynamic_labels += 1;
                    }
                    if text.is_empty() || scale.x <= 0. || scale.y <= 0. {
                        continue;
                    }
                    let (rotation, flip) = if let Some(context) = context {
                        (
                            (*rotation
                                + context.rotation
                                + if context.flip && rotation % 2 == 1 {
                                    2
                                } else {
                                    0
                                })
                                % 4,
                            *flip ^ context.flip,
                        )
                    } else {
                        (*rotation, *flip)
                    };
                    let text = text.replace('\t', "    ");
                    let run = TextRun::new(
                        text,
                        transform.point(*at),
                        rotation,
                        flip,
                        *scale,
                        properties.flag("hcenter"),
                        properties.flag("vcenter"),
                    );
                    let hidden = properties.flag("hide")
                        || (context.is_some() && properties.get("hide") == Some("instance"));
                    let layer = properties
                        .get("layer")
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(3);
                    self.items.push(Item {
                        bounds: run.bounds,
                        shape: Shape::Text(run),
                        layer,
                        style: Style::new(properties, false),
                        hidden,
                        owner,
                        wire_record: None,
                        primitive_record: None,
                    });
                }
                RecordKind::Instance {
                    symbol,
                    at,
                    rotation,
                    flip,
                    properties,
                } => {
                    let local = transform.then(Transform::new(*at, *rotation, *flip));
                    let index = self.instances.len();
                    if depth == 0 {
                        self.instance_records.insert(index, record_index);
                    }
                    self.instances.push(InstanceInfo {
                        symbol: symbol.clone(),
                        resolved: None,
                        embedded: false,
                        source_file: file.to_path_buf(),
                        definition: None,
                        at: local.origin,
                        rotation: *rotation,
                        flip: *flip,
                        properties: properties.clone(),
                        bounds: Bounds::new(local.origin, local.origin).expanded(10.),
                    });
                    let before = self.items.len();
                    let definition = if let Some(doc) = embedded.get(symbol.as_str()) {
                        self.instances[index].embedded = true;
                        Ok((
                            Arc::clone(doc),
                            file.to_path_buf(),
                            SymbolKey::Embedded(file.to_path_buf(), symbol.clone()),
                        ))
                    } else {
                        self.library
                            .paths
                            .resolve(
                                symbol,
                                file.parent().unwrap_or(Path::new(".")),
                                doc.version() == "1.0",
                            )
                            .and_then(|resolved| {
                                self.instances[index].resolved = Some(resolved.clone());
                                if !self.dependencies.contains(&resolved) {
                                    self.dependencies.push(resolved.clone());
                                }
                                self.library.document(&resolved).map(|doc| {
                                    (doc, resolved.clone(), SymbolKey::File(resolved.clone()))
                                })
                            })
                    };
                    if let Ok((definition, _, _)) = &definition {
                        self.instances[index].definition = Some(Arc::clone(definition));
                    }
                    match definition {
                        Ok((symbol_doc, symbol_file, key))
                            if depth < 32 && !self.active.contains(&key) =>
                        {
                            self.active.insert(key.clone());
                            let metadata = symbol_doc.symbol_properties();
                            let template = Properties::parse(
                                metadata
                                    .and_then(|p| p.get("template"))
                                    .unwrap_or("")
                                    .into(),
                            );
                            let pins = symbol_doc
                                .records
                                .iter()
                                .filter_map(|r| match &r.kind {
                                    RecordKind::Rectangle {
                                        layer: 5,
                                        properties,
                                        ..
                                    } => Some(properties),
                                    _ => None,
                                })
                                .collect();
                            let (parent_rot, parent_flip) =
                                context.map(|c| (c.rotation, c.flip)).unwrap_or((0, false));
                            let label_context = LabelContext {
                                instance: properties,
                                template,
                                symbol,
                                pins,
                                rotation: (parent_rot
                                    + if parent_flip {
                                        (4 - rotation) % 4
                                    } else {
                                        *rotation
                                    })
                                    % 4,
                                flip: parent_flip ^ flip,
                            };
                            self.visit(
                                &symbol_doc,
                                &symbol_file,
                                local,
                                Some(&label_context),
                                Some(owner.unwrap_or(index)),
                                depth + 1,
                            )?;
                            self.active.remove(&key);
                        }
                        Ok(_) => {
                            self.warn(
                                file,
                                record.line,
                                format!("cyclic or over-deep symbol reference {symbol:?}"),
                            );
                            self.missing(local.origin, symbol, owner.unwrap_or(index));
                        }
                        Err(error) => {
                            self.warn(file, record.line, error);
                            self.missing(local.origin, symbol, owner.unwrap_or(index));
                        }
                    }
                    if let Some(bounds) = self.items[before..]
                        .iter()
                        .filter(|item| !item.hidden)
                        .map(|item| item.bounds)
                        .reduce(|mut a, b| {
                            a.include(b);
                            a
                        })
                    {
                        self.instances[index].bounds = bounds;
                    }
                }
                _ => {}
            }
            if depth == 0
                && record.kind.is_editable()
                && !matches!(
                    record.kind,
                    RecordKind::Instance { .. } | RecordKind::Wire { .. }
                )
            {
                for item in &mut self.items[first_item..] {
                    item.primitive_record = Some(record_index);
                }
            }
        }
        Ok(())
    }
    fn missing(&mut self, at: Point, reference: &str, owner: usize) {
        let style = Style {
            width: 0.,
            bus: false,
            dash: 3.,
            fill: Fill::None,
        };
        let d = Point::new(15., 15.);
        self.path(vec![at - d, at + d], false, 7, style.clone(), Some(owner));
        self.path(
            vec![at + Point::new(-15., 15.), at + Point::new(15., -15.)],
            false,
            7,
            style.clone(),
            Some(owner),
        );
        let name = Path::new(reference)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let text = TextRun::new(
            format!("Missing: {name}"),
            at + Point::new(20., -10.),
            0,
            false,
            Point::new(0.2, 0.2),
            false,
            false,
        );
        self.items.push(Item {
            bounds: text.bounds,
            shape: Shape::Text(text),
            layer: 7,
            style,
            hidden: false,
            owner: Some(owner),
            wire_record: None,
            primitive_record: None,
        });
    }
    fn junctions(&mut self) -> Result<(), String> {
        for point in junction_points(&self.wires) {
            if self.items.len() >= 1_000_000 {
                return Err("scene exceeds one million drawing items".into());
            }
            self.path(
                arc_points(point, Point::new(2.5, 2.5), 0., 360., true),
                true,
                1,
                Style {
                    width: 0.,
                    bus: false,
                    dash: 0.,
                    fill: Fill::Solid,
                },
                None,
            );
        }
        Ok(())
    }
}

fn arc_points(center: Point, radius: Point, start: f64, sweep: f64, filled: bool) -> Vec<Point> {
    if !start.is_finite() || !sweep.is_finite() {
        return Vec::new();
    }
    let sweep = sweep.clamp(-360., 360.);
    let steps = ((sweep.abs() / 3.).ceil() as usize).clamp(2, 120);
    let mut points = Vec::new();
    if filled && sweep.abs() < 360. {
        points.push(center);
    }
    for i in 0..=steps {
        let angle = (start + sweep * i as f64 / steps as f64).to_radians();
        points.push(center + Point::new(radius.x * angle.cos(), -radius.y * angle.sin()));
    }
    points
}
fn bezier_points(points: &[Point]) -> Vec<Point> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut result = Vec::new();
    for i in 1..points.len() - 1 {
        let start = if i == 1 {
            points[0]
        } else {
            (points[i - 1] + points[i]) * 0.5
        };
        let end = if i == points.len() - 2 {
            points[i + 1]
        } else {
            (points[i] + points[i + 1]) * 0.5
        };
        for step in 0..=32 {
            let t = step as f64 / 32.;
            result
                .push(start * (1. - t).powi(2) + points[i] * (2. * t * (1. - t)) + end * t.powi(2));
        }
    }
    result
}

fn expand_label(text: &str, context: Option<&LabelContext<'_>>) -> (String, bool) {
    if text.contains("tcleval(") {
        return ("[Tcl expression]".into(), true);
    }
    let Some(context) = context else {
        return (text.into(), false);
    };
    let mut result = String::new();
    let mut unresolved = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '@' && c != '%' {
            result.push(c);
            continue;
        }
        let mut key = String::new();
        while chars
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '#' | ':'))
        {
            key.push(chars.next().unwrap());
        }
        if key.is_empty() {
            result.push(c);
            continue;
        }
        let pin_value = key
            .strip_prefix('#')
            .and_then(|s| s.split_once(':'))
            .and_then(|(index, field)| context.pins.get(index.parse::<usize>().ok()?)?.get(field));
        let value = context
            .instance
            .get(&key)
            .or_else(|| context.template.get(&key))
            .or(pin_value);
        if let Some(value) = value {
            result.push_str(value);
        } else if key == "symname" {
            result.push_str(
                &Path::new(context.symbol)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy(),
            );
        } else if key == "symname_ext" {
            result.push_str(context.symbol);
        } else {
            unresolved = true;
        }
    }
    (result, unresolved)
}

fn bits(value: f64) -> u64 {
    if value == 0. { 0 } else { value.to_bits() }
}
/// Geometric connection cues only, not a netlist/connectivity implementation.
/// An interior crossing without a wire endpoint does not create a junction.
pub fn junction_points(wires: &[(Point, Point)]) -> Vec<Point> {
    let mut endpoints: HashMap<(u64, u64), (Point, usize)> = HashMap::new();
    for &(a, b) in wires {
        if a == b {
            continue;
        }
        for p in [a, b] {
            endpoints.entry((bits(p.x), bits(p.y))).or_insert((p, 0)).1 += 1;
        }
    }
    let mut horizontal: HashMap<u64, Vec<Point>> = HashMap::new();
    let mut vertical: HashMap<u64, Vec<Point>> = HashMap::new();
    for &(p, _) in endpoints.values() {
        horizontal.entry(bits(p.y)).or_default().push(p);
        vertical.entry(bits(p.x)).or_default().push(p);
    }
    for points in horizontal.values_mut() {
        points.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
    for points in vertical.values_mut() {
        points.sort_by(|a, b| a.y.total_cmp(&b.y));
    }
    for &(a, b) in wires {
        if a == b {
            continue;
        }
        let (candidates, lo, hi, use_x) = if a.y == b.y {
            (horizontal.get(&bits(a.y)), a.x.min(b.x), a.x.max(b.x), true)
        } else if a.x == b.x {
            (vertical.get(&bits(a.x)), a.y.min(b.y), a.y.max(b.y), false)
        } else {
            continue;
        }; // diagonal wires are drawn, but do not yet supply interior junction cues
        if let Some(points) = candidates {
            let coord = |p: &Point| if use_x { p.x } else { p.y };
            let start = points.partition_point(|p| coord(p) <= lo);
            let end = points.partition_point(|p| coord(p) < hi);
            for p in &points[start..end] {
                endpoints.get_mut(&(bits(p.x), bits(p.y))).unwrap().1 += 2;
            }
        }
    }
    endpoints
        .into_values()
        .filter_map(|(p, degree)| (degree >= 3).then_some(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t_junction_is_connected_but_crossing_is_not() {
        let horizontal = (Point::new(-10., 0.), Point::new(10., 0.));
        let vertical = (Point::new(0., -10.), Point::new(0., 10.));
        assert!(junction_points(&[horizontal, vertical]).is_empty());
        assert_eq!(
            junction_points(&[horizontal, (Point::default(), Point::new(0., 10.))]),
            vec![Point::default()]
        );
    }
    #[test]
    fn degenerate_wires_do_not_corrupt_endpoint_ranges() {
        let origin = Point::default();
        let wires = [(origin, origin), (origin, Point::new(10., 0.))];
        assert!(junction_points(&wires).is_empty());
    }
    #[test]
    fn embedded_definition_is_used_without_external_library() {
        let source = "v {xschem file_version=1.3}\nC {a.sym} 100 200 1 1 {name=R9 value=2k}\n[\nK {template=\"name=R1 value=1k\"}\nL 4 0 0 10 20 {}\nT {@name @value} 0 0 0 0 .2 .2 {}\n]\nC {a.sym} 0 0 0 0 {name=R10}\n";
        let loaded = LoadedDocument::from_document(
            Arc::new(Document::parse(source).unwrap()),
            None,
            LibraryPaths::default(),
        )
        .unwrap();
        assert!(loaded.scene.diagnostics.is_empty());
        assert_eq!(loaded.scene.instances.len(), 2);
        assert!(
            loaded
                .scene
                .items
                .iter()
                .any(|i| matches!(&i.shape, Shape::Text(t) if t.text == "R9 2k"))
        );
        assert!(
            loaded
                .scene
                .items
                .iter()
                .any(|i| matches!(&i.shape, Shape::Text(t) if t.text == "R10 1k"))
        );
        assert!(loaded.scene.items.iter().any(|i| matches!(&i.shape, Shape::Path { points, .. } if points == &vec![Point::new(100., 200.), Point::new(80., 190.)])));
    }
}
