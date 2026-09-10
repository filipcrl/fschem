//! Xschem's record grammar, not a Tcl parser. In particular, outer braced
//! strings end at the first *unescaped* `}`; braces are not nested. Backslash
//! quotes the next character literally (backslash-n is not a newline).
use crate::geometry::Point;
use std::{fmt, ops::Range, sync::Arc};

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

#[derive(Clone, Debug)]
pub struct Document {
    source: Arc<str>,
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
            source,
            records,
            diagnostics,
        })
    }
    /// The compatibility contract for this read-only milestone: untouched data
    /// round-trips byte-for-byte, including CRLF, comments and unknown records.
    pub fn original(&self) -> &str {
        &self.source
    }
    pub fn raw_record(&self, record: &Record) -> &str {
        &self.source[record.span.clone()]
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
