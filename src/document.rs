//! Xschem's record grammar, not a Tcl parser. In particular, outer braced
//! strings end at the first *unescaped* `}`; braces are not nested. Backslash
//! quotes the next character literally (backslash-n is not a newline).
use crate::geometry::{Point, Transform, wire_route};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    ops::Range,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
    pub kind: DiagnosticKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    UnsupportedRecord,
    MalformedRecord,
    Structure,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}
impl std::error::Error for ParseError {}

/// Preserve order, duplicate keys, bare tokens and raw text. Rendering uses a
/// decoded lookup view, never a reserialized property map.
#[derive(Clone, Debug, Default)]
pub struct Properties {
    pub raw: String,
    pub entries: Vec<(String, Option<String>)>,
}
impl Properties {
    pub fn parse(raw: String) -> Self {
        let mut entries = Vec::new();
        let mut chars = raw.chars().peekable();
        while chars.peek().is_some() {
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            let mut key = String::new();
            while chars
                .peek()
                .is_some_and(|c| !c.is_whitespace() && *c != '=')
            {
                key.push(chars.next().unwrap());
            }
            if key.is_empty() {
                chars.next();
                continue;
            }
            if chars.peek() != Some(&'=') {
                entries.push((key, None));
                continue;
            }
            chars.next();
            let quoted = chars.peek() == Some(&'"');
            if quoted {
                chars.next();
            }
            let mut value = String::new();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        value.push(next);
                    } else {
                        value.push(c);
                    }
                } else if (quoted && c == '"') || (!quoted && c.is_whitespace()) {
                    break;
                } else {
                    value.push(c);
                }
            }
            entries.push((key, Some(value)));
        }
        Self { raw, entries }
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_deref())
    }
    pub fn flag(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|v| matches!(v, "true" | "1" | "yes" | "on"))
    }
    pub fn number(&self, key: &str) -> Option<f64> {
        self.get(key)?.parse::<f64>().ok().filter(|v| v.is_finite())
    }
}

#[derive(Clone, Debug)]
pub struct Record {
    /// Byte span in Document::source, including the record's trailing newline.
    pub span: Range<usize>,
    pub line: usize,
    pub kind: RecordKind,
}

#[derive(Clone, Debug)]
pub enum RecordKind {
    Header(Properties),
    Global {
        tag: char,
        properties: Properties,
    },
    Line {
        layer: u16,
        a: Point,
        b: Point,
        properties: Properties,
    },
    Wire {
        a: Point,
        b: Point,
        properties: Properties,
    },
    Rectangle {
        layer: u16,
        a: Point,
        b: Point,
        properties: Properties,
    },
    Arc {
        layer: u16,
        center: Point,
        radius: f64,
        start: f64,
        sweep: f64,
        properties: Properties,
    },
    Polygon {
        layer: u16,
        points: Vec<Point>,
        properties: Properties,
    },
    Text {
        text: String,
        at: Point,
        rotation: u8,
        flip: bool,
        scale: Point,
        properties: Properties,
    },
    Instance {
        symbol: String,
        at: Point,
        rotation: u8,
        flip: bool,
        properties: Properties,
    },
    Embedded(Vec<Record>),
    Comment,
    Unknown {
        tag: String,
    },
}
impl RecordKind {
    pub fn is_editable(&self) -> bool {
        matches!(
            self,
            Self::Instance { .. }
                | Self::Wire { .. }
                | Self::Line { .. }
                | Self::Rectangle { .. }
                | Self::Arc { .. }
                | Self::Polygon { .. }
                | Self::Text { .. }
        )
    }
}

#[derive(Clone, Debug)]
pub struct Document {
    source: Arc<str>,
    scope: Range<usize>,
    pub records: Vec<Record>,
    pub diagnostics: Vec<Diagnostic>,
}
impl Document {
    pub fn parse(source: impl Into<Arc<str>>) -> Result<Self, ParseError> {
        let source = source.into();
        let line_starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let mut parser = Parser {
            source: &source,
            pos: 0,
            diagnostics: Vec::new(),
            line_starts,
            record_count: 0,
        };
        if source.starts_with('\u{feff}') {
            parser.pos = '\u{feff}'.len_utf8();
        }
        let records = parser.records(false, 0)?;
        let diagnostics = parser.diagnostics;
        Ok(Self {
            scope: 0..source.len(),
            source,
            records,
            diagnostics,
        })
    }
    /// The compatibility contract: untouched data
    /// round-trips byte-for-byte, including CRLF, comments and unknown records.
    pub fn original(&self) -> &str {
        &self.source[self.scope.clone()]
    }
    pub fn raw_record(&self, record: &Record) -> &str {
        &self.source[record.span.clone()]
    }
    /// Replace a saved file only after its complete contents have been written.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        use std::{
            fs,
            io::Write,
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT_SAVE: AtomicU64 = AtomicU64::new(0);
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let (temporary, mut file) = loop {
            let temporary = parent.join(format!(
                ".fschem-save-{}-{}.tmp",
                std::process::id(),
                NEXT_SAVE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => break (temporary, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
        };
        let result = (|| -> std::io::Result<()> {
            file.write_all(self.original().as_bytes())?;
            if let Ok(metadata) = fs::metadata(path) {
                file.set_permissions(metadata.permissions())?;
            }
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| format!("{}: {error}", path.display()))
    }
    /// Duplicate editable records in this document, assigning fresh instance names.
    /// Embedded definitions remain available to their copied symbol references.
    pub fn with_copied_records(
        &self,
        records: &BTreeSet<usize>,
    ) -> Result<(Self, BTreeSet<usize>), String> {
        let mut names: BTreeSet<_> = self
            .records
            .iter()
            .filter_map(|record| match &record.kind {
                RecordKind::Instance { properties, .. }
                | RecordKind::Rectangle {
                    layer: 5,
                    properties,
                    ..
                } => properties.get("name").map(str::to_ascii_lowercase),
                _ => None,
            })
            .collect();
        let mut appended = String::new();
        for &index in records {
            let record = self
                .records
                .get(index)
                .ok_or("Selected record is missing")?;
            let mut raw = self.raw_record(record).to_owned();
            match &record.kind {
                RecordKind::Instance { properties, .. }
                | RecordKind::Rectangle {
                    layer: 5,
                    properties,
                    ..
                } => {
                    let prefix = properties
                        .get("name")
                        .unwrap_or(if matches!(record.kind, RecordKind::Rectangle { .. }) {
                            "P"
                        } else {
                            "X"
                        })
                        .trim_end_matches(|c: char| c.is_ascii_digit());
                    let prefix = if prefix.is_empty() { "X" } else { prefix };
                    let mut number = 1;
                    let name = loop {
                        let name = format!("{prefix}{number}");
                        if names.insert(name.to_ascii_lowercase()) {
                            break name;
                        }
                        number += 1;
                    };
                    // Only the copy's property field is serialized; original bytes are untouched.
                    let entries = std::iter::once(("name", Some(name.as_str()))).chain(
                        properties
                            .entries
                            .iter()
                            .filter(|(key, _)| key != "name")
                            .map(|(key, value)| (key.as_str(), value.as_deref())),
                    );
                    let properties = entries
                        .map(|(key, value)| match value {
                            Some(value) => format!(
                                "{key}=\"{}\"",
                                value.replace('\\', "\\\\").replace('"', "\\\"")
                            ),
                            None => key.to_owned(),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let mut parser = Parser {
                        source: &raw,
                        pos: 0,
                        diagnostics: Vec::new(),
                        line_starts: std::iter::once(0)
                            .chain(raw.match_indices('\n').map(|(i, _)| i + 1))
                            .collect(),
                        record_count: 0,
                    };
                    parser.bump();
                    for _ in 0..5 {
                        parser.token().map_err(|e| e.to_string())?;
                    }
                    parser.whitespace();
                    let start = parser.pos;
                    parser.token().map_err(|e| e.to_string())?;
                    let end = parser.pos;
                    let escaped = properties
                        .replace('\\', "\\\\")
                        .replace('{', "\\{")
                        .replace('}', "\\}");
                    raw.replace_range(start..end, &format!("{{{escaped}}}"));
                }
                kind if kind.is_editable() => {}
                _ => return Err("This record cannot be copied".into()),
            }
            appended.push_str(&raw);
            if !appended.ends_with('\n') {
                appended.push_str(self.newline());
            }
        }
        let copied = (self.records.len()..self.records.len() + records.len()).collect();
        Ok((self.with_appended_text(&appended)?, copied))
    }
    /// Remove selected records, retaining shared embedded definitions for surviving instances.
    pub fn without_records(&self, records: &BTreeSet<usize>) -> Result<Self, String> {
        if records.is_empty() {
            return Ok(self.clone());
        }
        for &index in records {
            if !self
                .records
                .get(index)
                .is_some_and(|record| record.kind.is_editable())
            {
                return Err("This record cannot be deleted".into());
            }
        }
        let mut removed = records.clone();
        let mut relocated: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut definitions = BTreeSet::new();
        let mut owner = None;
        for (index, record) in self.records.iter().enumerate() {
            match &record.kind {
                RecordKind::Instance { symbol, .. } => owner = Some((index, symbol)),
                RecordKind::Embedded(_) => {
                    let Some((instance, symbol)) = owner else {
                        continue;
                    };
                    let first = definitions.insert(symbol);
                    if records.contains(&instance) {
                        removed.insert(index);
                        if first && let Some((survivor, _)) = self.records.iter().enumerate().find(|(i, record)| {
                            !records.contains(i) && matches!(&record.kind, RecordKind::Instance { symbol: other, .. } if other == symbol)
                        }) {
                            relocated.entry(survivor).or_default().push(index);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut source = String::with_capacity(self.original().len());
        let mut cursor = self.scope.start;
        for (index, record) in self.records.iter().enumerate() {
            if removed.contains(&index) {
                source.push_str(&self.source[cursor..record.span.start]);
                cursor = record.span.end;
            } else if let Some(definitions) = relocated.get(&index) {
                source.push_str(&self.source[cursor..record.span.end]);
                for &definition in definitions {
                    if !source.ends_with('\n') {
                        source.push_str(self.newline());
                    }
                    source.push_str(self.raw_record(&self.records[definition]));
                }
                cursor = record.span.end;
            }
        }
        source.push_str(&self.source[cursor..self.scope.end]);
        Self::parse(source).map_err(|e| e.to_string())
    }
    pub fn with_wire(&self, a: Point, b: Point) -> Result<Self, String> {
        if [a.x, a.y, b.x, b.y]
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1e100)
        {
            return Err("Wire position is outside the supported range".into());
        }
        if a == b {
            return Ok(self.clone());
        }
        let text = wire_route(a, b)
            .windows(2)
            .map(|segment| {
                let [a, b] = [segment[0], segment[1]];
                format!("N {} {} {} {} {{}}{}", a.x, a.y, b.x, b.y, self.newline())
            })
            .collect::<String>();
        self.with_appended_text(&text)
    }
    fn newline(&self) -> &'static str {
        if self.original().contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }
    fn with_appended_text(&self, text: &str) -> Result<Self, String> {
        let mut source = self.original().to_owned();
        if !source.is_empty() && !source.ends_with('\n') {
            source.push_str(self.newline());
        }
        source.push_str(text);
        Self::parse(source).map_err(|e| e.to_string())
    }
    /// Edit only an instance's numeric transform tokens, preserving all other bytes.
    pub fn with_instance_transform(
        &self,
        index: usize,
        at: Point,
        rotation: u8,
        flip: bool,
    ) -> Result<Self, String> {
        let record = self
            .records
            .get(index)
            .ok_or("Instance record is missing")?;
        if !matches!(record.kind, RecordKind::Instance { .. }) {
            return Err("Record is not an instance".into());
        }
        if !at.x.is_finite() || !at.y.is_finite() || at.x.abs().max(at.y.abs()) > 1e100 {
            return Err("Instance position is outside the supported range".into());
        }
        self.with_numeric_edits(vec![(
            index,
            vec![at.x, at.y, (rotation % 4) as f64, u8::from(flip) as f64],
        )])
    }
    /// Apply a rigid group transform to schematic and symbol drawing records.
    pub fn with_transformed_records(
        &self,
        records: &BTreeSet<usize>,
        pivot: Point,
        at: Point,
        rotation: u8,
        flip: bool,
    ) -> Result<Self, String> {
        let transform = Transform::new(at, rotation, flip);
        let mut values = Vec::new();
        for &index in records {
            let record = self
                .records
                .get(index)
                .ok_or("Selected record is missing")?;
            let numbers = match &record.kind {
                RecordKind::Instance {
                    at: old,
                    rotation: old_rotation,
                    flip: old_flip,
                    ..
                }
                | RecordKind::Text {
                    at: old,
                    rotation: old_rotation,
                    flip: old_flip,
                    ..
                } => {
                    let point = transform.point(*old - pivot);
                    let rotation = (rotation % 4
                        + if flip {
                            (4 - old_rotation) % 4
                        } else {
                            *old_rotation
                        })
                        % 4;
                    vec![
                        point.x,
                        point.y,
                        rotation as f64,
                        u8::from(flip ^ *old_flip) as f64,
                    ]
                }
                RecordKind::Wire { a, b, .. }
                | RecordKind::Line { a, b, .. }
                | RecordKind::Rectangle { a, b, .. } => {
                    let a = transform.point(*a - pivot);
                    let b = transform.point(*b - pivot);
                    vec![a.x, a.y, b.x, b.y]
                }
                RecordKind::Polygon { points, .. } => points
                    .iter()
                    .flat_map(|point| {
                        let point = transform.point(*point - pivot);
                        [point.x, point.y]
                    })
                    .collect(),
                RecordKind::Arc {
                    center,
                    radius,
                    start,
                    sweep,
                    ..
                } => {
                    let center = transform.point(*center - pivot);
                    let start = ((if flip { 180. - start } else { *start })
                        - (rotation % 4) as f64 * 90.)
                        .rem_euclid(360.);
                    vec![
                        center.x,
                        center.y,
                        *radius,
                        start,
                        if flip { -*sweep } else { *sweep },
                    ]
                }
                _ => return Err("This record cannot be moved".into()),
            };
            values.push((index, numbers));
        }
        self.with_numeric_edits(values)
    }
    fn with_numeric_edits(&self, values: Vec<(usize, Vec<f64>)>) -> Result<Self, String> {
        let mut edits = Vec::new();
        let mut parser = Parser {
            source: &self.source,
            pos: 0,
            diagnostics: Vec::new(),
            line_starts: std::iter::once(0)
                .chain(self.source.match_indices('\n').map(|(i, _)| i + 1))
                .collect(),
            record_count: 0,
        };
        for (index, numbers) in values {
            if numbers.iter().any(|v| !v.is_finite() || v.abs() > 1e100) {
                return Err("Position is outside the supported range".into());
            }
            let record = &self.records[index];
            parser.pos = record.span.start;
            parser.bump(); // Record tags need not be separated from numeric fields.
            let skip = match record.kind {
                RecordKind::Wire { .. } => 0,
                RecordKind::Polygon { .. } => 2,
                _ => 1, // Symbol/text string or drawing layer.
            };
            for _ in 0..skip {
                parser.token().map_err(|e| e.to_string())?;
            }
            for value in numbers {
                parser.whitespace();
                let start = parser.pos;
                parser.token().map_err(|e| e.to_string())?;
                edits.push((start..parser.pos, value.to_string()));
            }
        }
        edits.sort_by_key(|(span, _)| span.start);
        let mut source = String::with_capacity(self.source.len());
        let mut cursor = 0;
        for (span, value) in edits {
            source.push_str(&self.source[cursor..span.start]);
            source.push_str(&value);
            cursor = span.end;
        }
        source.push_str(&self.source[cursor..]);
        let end = self.scope.end + source.len() - self.source.len();
        Self::parse(source[self.scope.start..end].to_owned()).map_err(|e| e.to_string())
    }
    pub fn global(&self, tag: char) -> Option<&Properties> {
        self.records
            .iter()
            .rev()
            .find_map(|record| match &record.kind {
                RecordKind::Global { tag: t, properties } if *t == tag => Some(properties),
                _ => None,
            })
    }
    pub fn version(&self) -> &str {
        self.records
            .iter()
            .find_map(|record| match &record.kind {
                RecordKind::Header(p) => p.get("file_version"),
                _ => None,
            })
            .unwrap_or("1.0")
    }
    pub fn symbol_properties(&self) -> Option<&Properties> {
        self.global('K')
            .filter(|p| !p.raw.trim().is_empty())
            .or_else(|| self.global('G').filter(|p| p.get("type").is_some()))
    }
    pub fn embedded(&self, records: &[Record]) -> Self {
        Self {
            source: Arc::clone(&self.source),
            scope: records.first().map_or(0, |r| r.span.start)
                ..records.last().map_or(0, |r| r.span.end),
            records: records.to_vec(),
            diagnostics: Vec::new(),
        }
    }
}

struct Token {
    value: String,
    braced: bool,
}
struct Parser<'a> {
    source: &'a str,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    line_starts: Vec<usize>,
    record_count: usize,
}
impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> ParseError {
        let line = self.line_starts.partition_point(|start| *start <= self.pos);
        ParseError {
            line,
            column: self.source[self.line_starts[line - 1]..self.pos]
                .chars()
                .count()
                + 1,
            message: message.into(),
        }
    }
    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
    fn whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }
    fn token(&mut self) -> Result<Token, ParseError> {
        self.whitespace();
        if self.peek().is_none() {
            return Err(self.error("unexpected end of file"));
        }
        let braced = self.peek() == Some('{');
        let mut value = String::new();
        if braced {
            self.bump();
            let start = self.pos;
            loop {
                match self.bump() {
                    Some('}') => break,
                    Some('\r') => {}
                    Some('\\') => {
                        // Xschem ignores CR even between a backslash and its escaped character.
                        while self.peek() == Some('\r') {
                            self.bump();
                        }
                        value.push(
                            self.bump()
                                .ok_or_else(|| self.error("unfinished escape in braced string"))?,
                        );
                    }
                    Some(c) => value.push(c),
                    None => {
                        self.pos = start;
                        return Err(self.error("unterminated braced string"));
                    }
                }
            }
        } else {
            while self
                .peek()
                .is_some_and(|c| !c.is_whitespace() && c != '{' && c != '}')
            {
                value.push(self.bump().unwrap());
            }
        }
        Ok(Token { value, braced })
    }
    fn string(&mut self) -> Result<String, ParseError> {
        let token = self.token()?;
        if !token.braced {
            return Err(self.error("expected a braced string"));
        }
        Ok(token.value)
    }
    fn properties(&mut self) -> Result<Properties, ParseError> {
        Ok(Properties::parse(self.string()?))
    }
    fn number(&mut self) -> Result<f64, ParseError> {
        let token = self.token()?;
        let value = token
            .value
            .parse::<f64>()
            .map_err(|_| self.error(format!("invalid number {:?}", token.value)))?;
        if token.braced || !value.is_finite() || value.abs() > 1e100 {
            return Err(self.error("expected a finite number within the supported numeric range"));
        }
        Ok(value)
    }
    fn integer(&mut self, max: u32) -> Result<u32, ParseError> {
        let token = self.token()?;
        let value = token
            .value
            .parse::<u32>()
            .map_err(|_| self.error(format!("invalid integer {:?}", token.value)))?;
        if token.braced || value > max {
            return Err(self.error(format!("integer exceeds {max}")));
        }
        Ok(value)
    }
    fn point(&mut self) -> Result<Point, ParseError> {
        Ok(Point::new(self.number()?, self.number()?))
    }
    fn finish_line(&mut self) {
        while let Some(c) = self.bump() {
            if c == '\n' {
                break;
            }
        }
    }
    fn skip_record(&mut self) -> Result<(), ParseError> {
        while let Some(c) = self.peek() {
            if c == '\n' {
                self.bump();
                break;
            }
            if c == '{' {
                self.string()?;
            } else {
                self.bump();
            }
        }
        Ok(())
    }
    fn records(&mut self, embedded: bool, depth: usize) -> Result<Vec<Record>, ParseError> {
        if depth > 64 {
            return Err(self.error("embedded symbol nesting exceeds 64"));
        }
        let mut records = Vec::new();
        let mut has_instance = false;
        loop {
            self.whitespace();
            let start = self.pos;
            if self.peek().is_none() {
                if embedded {
                    return Err(self.error("missing ] after embedded symbol"));
                }
                break;
            }
            let line = self.error("").line;
            self.record_count += 1;
            if self.record_count > 1_000_000 {
                return Err(self.error("document exceeds one million records"));
            }
            // A comment consumes its whole line, including any unmatched braces.
            if self.peek() == Some('#') {
                self.finish_line();
                records.push(Record {
                    span: start..self.pos,
                    line,
                    kind: RecordKind::Comment,
                });
                continue;
            }
            // Tags are single characters; handwritten files can use K{...}.
            let tag = self.bump().expect("nonempty input").to_string();
            if tag == "]" && embedded {
                self.finish_line();
                break;
            }
            let checkpoint = self.pos;
            let kind = if tag == "[" {
                self.finish_line();
                let children = self.records(true, depth + 1)?;
                if !has_instance {
                    self.diagnostics.push(Diagnostic {
                        line,
                        message: "embedded definition has no preceding instance".into(),
                        kind: DiagnosticKind::Structure,
                    });
                }
                RecordKind::Embedded(children)
            } else {
                match self.known(&tag) {
                    Ok(Some(kind)) => {
                        self.finish_line();
                        kind
                    }
                    Ok(None) => {
                        if tag == "{" {
                            self.pos = start;
                        }
                        self.skip_record()?;
                        self.diagnostics.push(Diagnostic {
                            line,
                            message: format!("unrecognized record {tag:?}; preserved verbatim"),
                            kind: DiagnosticKind::UnsupportedRecord,
                        });
                        RecordKind::Unknown { tag }
                    }
                    Err(error) => {
                        self.pos = checkpoint;
                        self.skip_record()?;
                        self.diagnostics.push(Diagnostic {
                            line,
                            message: format!(
                                "cannot interpret {tag} record: {}; preserved verbatim",
                                error.message
                            ),
                            kind: DiagnosticKind::MalformedRecord,
                        });
                        RecordKind::Unknown { tag }
                    }
                }
            };
            if matches!(&kind, RecordKind::Instance { .. }) {
                has_instance = true;
            }
            records.push(Record {
                span: start..self.pos,
                line,
                kind,
            });
        }
        Ok(records)
    }
    fn known(&mut self, tag: &str) -> Result<Option<RecordKind>, ParseError> {
        Ok(Some(match tag {
            "v" => RecordKind::Header(self.properties()?),
            "G" | "K" | "V" | "S" | "F" | "E" => RecordKind::Global {
                tag: tag.chars().next().unwrap(),
                properties: self.properties()?,
            },
            "L" | "B" => {
                let layer = self.integer(u16::MAX as u32)? as u16;
                let a = self.point()?;
                let b = self.point()?;
                let properties = self.properties()?;
                if tag == "L" {
                    RecordKind::Line {
                        layer,
                        a,
                        b,
                        properties,
                    }
                } else {
                    RecordKind::Rectangle {
                        layer,
                        a,
                        b,
                        properties,
                    }
                }
            }
            "N" => RecordKind::Wire {
                a: self.point()?,
                b: self.point()?,
                properties: self.properties()?,
            },
            "A" => RecordKind::Arc {
                layer: self.integer(u16::MAX as u32)? as u16,
                center: self.point()?,
                radius: self.number()?,
                start: self.number()?,
                sweep: self.number()?,
                properties: self.properties()?,
            },
            "P" => {
                let layer = self.integer(u16::MAX as u32)? as u16;
                let count = self.integer(1_000_000)?;
                let mut points = Vec::new();
                for _ in 0..count {
                    points.push(self.point()?);
                }
                RecordKind::Polygon {
                    layer,
                    points,
                    properties: self.properties()?,
                }
            }
            "C" | "T" => {
                let value = self.string()?;
                let at = self.point()?;
                let rotation = self.integer(3)? as u8;
                let flip = self.integer(1)? == 1;
                if tag == "C" {
                    RecordKind::Instance {
                        symbol: value,
                        at,
                        rotation,
                        flip,
                        properties: self.properties()?,
                    }
                } else {
                    RecordKind::Text {
                        text: value,
                        at,
                        rotation,
                        flip,
                        scale: self.point()?,
                        properties: self.properties()?,
                    }
                }
            }
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deleting_an_embedded_instance_preserves_the_definition_for_its_copies() {
        let source = "# keep\r\nv {xschem file_version=1.3}\r\nC {a.sym} 0 0 0 0 {name=X1}\r\n# embedded definition\r\n[\r\nL 4 0 0 10 10 {}\r\n]\r\nC {b.sym} 0 0 0 0 {name=X2}\r\nC {a.sym} 40 0 0 0 {name=X3}\r\nN 0 0 40 0 {}\r\n";
        let doc = Document::parse(source).unwrap();
        let deleted = doc.without_records(&BTreeSet::from([2, 7])).unwrap();
        assert!(deleted.diagnostics.is_empty());
        assert!(!deleted.original().contains("name=X1"));
        assert!(!deleted.original().contains("N 0 0"));
        assert!(
            deleted
                .original()
                .starts_with("# keep\r\nv {xschem file_version=1.3}\r\n# embedded definition\r\n")
        );
        let embedded_index = deleted
            .records
            .iter()
            .position(|r| matches!(r.kind, RecordKind::Embedded(_)))
            .unwrap();
        assert!(
            matches!(&deleted.records[embedded_index - 1].kind, RecordKind::Instance { symbol, .. } if symbol == "a.sym")
        );
        let all_instances = deleted
            .records
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, RecordKind::Instance { .. }))
            .map(|(i, _)| i)
            .collect();
        let empty = deleted.without_records(&all_instances).unwrap();
        assert!(empty.diagnostics.is_empty());
        assert!(
            !empty
                .records
                .iter()
                .any(|r| matches!(r.kind, RecordKind::Embedded(_)))
        );
        assert_eq!(doc.original(), source);
    }
    #[test]
    fn symbol_primitives_transform_without_changing_metadata_or_text() {
        let doc = Document::parse("v {xschem file_version=1.3}\nK {type=subcircuit template=\"name=X1\"}\nL 4 10 20 30 40 {dash=2}\nB 5 -2 -2 2 2 {name=P1 dir=in}\nA 4 50 60 10 30 120 {}\nP 4 3 0 0 10 0 10 20 {fill=full}\nT {label \\{x\\}} 70 80 0 0 .2 .3 {}\n").unwrap();
        let selected = BTreeSet::from([2, 3, 4, 5, 6]);
        for rotation in 0..4 {
            for flip in [false, true] {
                let transform = Transform::new(Point::new(100., 200.), rotation, flip);
                let edited = doc
                    .with_transformed_records(
                        &selected,
                        Point::default(),
                        transform.origin,
                        rotation,
                        flip,
                    )
                    .unwrap();
                assert!(edited.diagnostics.is_empty());
                assert_eq!(
                    edited.raw_record(&edited.records[1]),
                    doc.raw_record(&doc.records[1])
                );
                let RecordKind::Line { a, b, .. } = edited.records[2].kind else {
                    panic!("line expected")
                };
                assert_eq!(a, transform.point(Point::new(10., 20.)));
                assert_eq!(b, transform.point(Point::new(30., 40.)));
                let RecordKind::Polygon { points, .. } = &edited.records[5].kind else {
                    panic!("polygon expected")
                };
                assert_eq!(points[2], transform.point(Point::new(10., 20.)));
                let RecordKind::Text {
                    text,
                    at,
                    rotation: r,
                    flip: f,
                    scale,
                    ..
                } = &edited.records[6].kind
                else {
                    panic!("text expected")
                };
                assert_eq!(text, "label {x}");
                assert_eq!(*at, transform.point(Point::new(70., 80.)));
                assert_eq!((*r, *f, *scale), (rotation, flip, Point::new(0.2, 0.3)));
                let RecordKind::Arc {
                    center,
                    radius,
                    start,
                    sweep,
                    ..
                } = edited.records[4].kind
                else {
                    panic!("arc expected")
                };
                for fraction in [0., 0.5, 1.] {
                    let angle = (30_f64 + 120. * fraction).to_radians();
                    let expected = transform
                        .point(Point::new(50. + 10. * angle.cos(), 60. - 10. * angle.sin()));
                    let angle = (start + sweep * fraction).to_radians();
                    let actual = center + Point::new(radius * angle.cos(), -radius * angle.sin());
                    assert!((actual - expected).length() < 1e-9);
                }
            }
        }
    }
    #[test]
    fn copying_preserves_original_text_and_property_values_with_unique_names() {
        let source = r#"v {xschem file_version=1.3}
C {r.sym} 0 0 0 0 {name=R1 value="say \\"hi\\"" path="a\\\\b" custom=one custom=two flag}
[
L 4 0 0 20 0 {}
]
C {r.sym} 40 0 0 0 {name=r2}
N0 0 40 0 {lab=signal}
"#
        .replace('\n', "\r\n");
        let doc = Document::parse(source.clone()).unwrap();
        assert!(doc.diagnostics.is_empty());
        let (copied, indices) = doc.with_copied_records(&BTreeSet::from([1, 4])).unwrap();
        assert_eq!(indices, BTreeSet::from([5, 6]));
        assert!(copied.original().starts_with(&source));
        assert_eq!(doc.original(), source);
        let RecordKind::Instance {
            properties: original,
            ..
        } = &doc.records[1].kind
        else {
            panic!("instance expected")
        };
        let RecordKind::Instance {
            properties: clone, ..
        } = &copied.records[5].kind
        else {
            panic!("instance expected")
        };
        assert_eq!(clone.get("name"), Some("R3"));
        assert_eq!(
            original
                .entries
                .iter()
                .filter(|(key, _)| key != "name")
                .collect::<Vec<_>>(),
            clone
                .entries
                .iter()
                .filter(|(key, _)| key != "name")
                .collect::<Vec<_>>()
        );
        assert_eq!(
            copied.raw_record(&copied.records[6]),
            doc.raw_record(&doc.records[4])
        );
        let (again, _) = copied.with_copied_records(&BTreeSet::from([1])).unwrap();
        let RecordKind::Instance { properties, .. } = &again.records[7].kind else {
            panic!("instance expected")
        };
        assert_eq!(properties.get("name"), Some("R4"));
    }
    #[test]
    fn adding_wire_preserves_source_and_rejects_invalid_or_zero_length_segments() {
        let doc = Document::parse("# retained comment without a newline").unwrap();
        let wired = doc
            .with_wire(Point::new(1., 2.), Point::new(3., 4.))
            .unwrap();
        assert_eq!(
            wired.original(),
            "# retained comment without a newline\nN 1 2 3 4 {}\n"
        );
        assert_eq!(wired.records.len(), 2);
        assert_eq!(
            doc.with_wire(Point::default(), Point::default())
                .unwrap()
                .original(),
            doc.original()
        );
        assert!(
            doc.with_wire(Point::new(f64::NAN, 0.), Point::default())
                .is_err()
        );
    }
    #[test]
    fn instance_edit_preserves_surrounding_bytes_and_embedded_scope() {
        let source = "# keep\r\nC{odd\\}name.sym}10\t20\r\n0 0{name=X1 custom=one custom=two}\r\n[\r\nv {xschem file_version=1.3}\r\nC {child.sym} 0 0 0 0 {}\r\n]\r\nZ {future}\r\n";
        let doc = Document::parse(source).unwrap();
        let edited = doc
            .with_instance_transform(1, Point::new(-40., 60.), 3, true)
            .unwrap();
        assert_eq!(
            edited.original(),
            source.replacen("10\t20\r\n0 0", "-40\t60\r\n3 1", 1)
        );
        assert_eq!(doc.original(), source);
        let RecordKind::Embedded(records) = &doc.records[2].kind else {
            panic!("embedded expected")
        };
        let embedded = doc.embedded(records);
        let edited = embedded
            .with_instance_transform(1, Point::new(20., -20.), 1, false)
            .unwrap();
        assert_eq!(
            edited.original(),
            "v {xschem file_version=1.3}\r\nC {child.sym} 20 -20 1 0 {}\r\n"
        );
        assert_eq!(edited.records.len(), 2);
    }
    #[test]
    fn preserves_original_bytes_and_unknown_data() {
        let source = "\u{feff}v {xschem version=3.4.8 file_version=1.3}\r\n# { not a field\r\nF {parameters\r\nx=3}\r\nZ {future\r\nrecord} 5\r\nN 0 1 2 3 {custom=\"a b\"}\r\n  ";
        let doc = Document::parse(source).unwrap();
        assert_eq!(doc.original().as_bytes(), source.as_bytes());
        assert_eq!(doc.global('F').unwrap().raw, "parameters\nx=3");
        assert_eq!(doc.diagnostics.len(), 1);
        assert!(matches!(
            doc.records.last().unwrap().kind,
            RecordKind::Wire { .. }
        ));
    }
    #[test]
    fn decodes_outer_and_property_escaping_separately() {
        let doc =
            Document::parse(r#"T {a\{b\} \\ n=\n} 0 0 0 0 .2 .2 {value="say \\"hi\\""}"#).unwrap();
        let RecordKind::Text {
            text, properties, ..
        } = &doc.records[0].kind
        else {
            panic!("text expected");
        };
        assert_eq!(text, "a{b} \\ n=n");
        assert_eq!(properties.get("value"), Some("say \"hi\""));
    }
    #[test]
    fn preserves_embedded_symbols_and_recovers_bad_record() {
        let text = "C {missing.sym} 0 0 1 1 {}\n[\nv {xschem file_version=1.2}\nK {type=resistor}\nL 4 -1 0 1 0 {}\n]\nN nan 0 0 0 {}\nN 0 0 1 1 {}\n";
        let doc = Document::parse(text).unwrap();
        assert!(matches!(&doc.records[1].kind, RecordKind::Embedded(v) if v.len() == 3));
        assert_eq!(doc.diagnostics.len(), 1);
        assert!(matches!(doc.records[3].kind, RecordKind::Wire { .. }));
        assert_eq!(doc.original(), text);
        assert!(Document::parse("K {unterminated").is_err());
        assert!(Document::parse("[\nK {}\n").is_err());
    }
    #[test]
    fn numeric_fields_can_wrap_across_lines() {
        let doc = Document::parse("P 4 3 0 0\n10 0\n10 10 {fill=full}\n").unwrap();
        assert!(
            matches!(&doc.records[0].kind, RecordKind::Polygon { points, .. } if points.len() == 3)
        );
    }
    #[test]
    fn record_tags_and_braced_fields_need_no_separating_space() {
        let source = "v{xschem file_version=1.3}\nN0 0 10 0{}\nC{r.sym}10 0 0 0{name=R1}\n";
        let doc = Document::parse(source).unwrap();
        assert_eq!(doc.records.len(), 3);
        assert!(doc.diagnostics.is_empty());
        assert_eq!(doc.version(), "1.3");
        assert_eq!(doc.original(), source);
    }
}
