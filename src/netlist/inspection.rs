//! Versioned read-only JSON views of real compiler stages.
use super::*;
use crate::document::{DiagnosticKind, Record};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Serialize a deterministic snapshot without executing code or simulators.
/// Config is a JSON object and is accepted only for the lowered stage.
pub fn dump_ir(
    doc: &Document,
    source: &Path,
    paths: LibraryPaths,
    stage: &str,
    backend: &str,
    config: &str,
) -> Result<String, String> {
    if !matches!(backend, "ngspice" | "vacask") {
        return Err(format!("unsupported backend {backend}"));
    }
    let config: Config =
        serde_json::from_str(config).map_err(|e| format!("invalid IR config: {e}"))?;
    if stage != "lowered" && !config.empty() {
        return Err("portable configuration is supported only for lowered IR".into());
    }
    let mut value = match stage {
        "parsed" => parsed(doc)?,
        "resolved" => {
            let mut inspector = Resolver {
                library: Library::new(paths),
                instances: Vec::new(),
                sheets: Vec::new(),
            };
            inspector.sheet(doc, source, "", None, 0)?;
            inspector
                .instances
                .sort_by_key(|v| v["id"].as_str().unwrap().to_owned());
            inspector
                .sheets
                .sort_by_key(|v| v["id"].as_str().unwrap().to_owned());
            json!({"instances":inspector.instances,"sheets":inspector.sheets,"ignore_policy":"portable: backend ignore flags are retained, not applied"})
        }
        "lowered" => {
            let circuit = portable::elaborate(doc, source, paths, backend, config.options()?)?;
            lowered_snapshot(&circuit, source, backend)
        }
        _ => {
            return Err(format!(
                "unknown IR stage {stage:?}; expected parsed, resolved or lowered"
            ));
        }
    };
    value["schema_version"] = json!(1);
    value["stage"] = json!(stage);
    value["source"] = json!(source);
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
    #[serde(default)]
    local_parameters: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    exclude: std::collections::BTreeSet<String>,
    #[serde(default)]
    sources: BTreeMap<String, Source>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    kind: String,
    nodes: Vec<String>,
    value: Value,
}
impl Config {
    fn empty(&self) -> bool {
        self.parameters.is_empty()
            && self.local_parameters.is_empty()
            && self.exclude.is_empty()
            && self.sources.is_empty()
    }
    fn options(self) -> Result<portable::Options, String> {
        let expr = |value: Value| -> Result<String, String> {
            match value {
                Value::String(s) => Ok(s),
                Value::Number(n) => Ok(n.to_string()),
                _ => Err("parameter/source values must be strings or numbers".into()),
            }
        };
        let defs = |values: BTreeMap<String, Value>| {
            values
                .into_iter()
                .map(|(k, v)| Ok((k, expr(v)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()
        };
        Ok(portable::Options {
            parameters: defs(self.parameters)?,
            local_parameters: self
                .local_parameters
                .into_iter()
                .map(|(k, v)| Ok((k, defs(v)?)))
                .collect::<Result<_, String>>()?,
            exclude: self.exclude,
            sources: self
                .sources
                .into_iter()
                .map(|(k, v)| Ok((k, (v.kind, v.nodes, expr(v.value)?))))
                .collect::<Result<_, String>>()?,
        })
    }
}
fn properties(p: &Properties) -> Value {
    json!({"raw":p.raw,"entries":p.entries.iter().map(|(key,value)|json!({"key":key,"value":value})).collect::<Vec<_>>()})
}
fn point(p: &Point) -> Value {
    json!([p.x, p.y])
}
fn parsed(doc: &Document) -> Result<Value, String> {
    if !doc.diagnostics.is_empty() {
        return Err(format!(
            "parsed IR unavailable: {}",
            doc.diagnostics
                .iter()
                .map(|d| format!(
                    "line {}: {} ({})",
                    d.line,
                    d.message,
                    match d.kind {
                        DiagnosticKind::UnsupportedRecord => "unsupported record",
                        DiagnosticKind::MalformedRecord => "malformed record",
                        DiagnosticKind::Structure => "structure",
                    }
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    Ok(
        json!({"text":doc.original(),"records":doc.records.iter().map(|r|record(doc,r)).collect::<Vec<_>>()}),
    )
}
fn record(doc: &Document, r: &Record) -> Value {
    let mut v = match &r.kind {
        RecordKind::Header(p) => json!({"kind":"header","properties":properties(p)}),
        RecordKind::Global { tag, properties: p } => {
            json!({"kind":"global","tag":tag,"properties":properties(p)})
        }
        RecordKind::Line {
            layer,
            a,
            b,
            properties: p,
        }
        | RecordKind::Rectangle {
            layer,
            a,
            b,
            properties: p,
        } => {
            json!({"kind":if matches!(r.kind,RecordKind::Line{..}){"line"}else{"rectangle"},"layer":layer,"a":point(a),"b":point(b),"properties":properties(p)})
        }
        RecordKind::Wire {
            a,
            b,
            properties: p,
        } => json!({"kind":"wire","a":point(a),"b":point(b),"properties":properties(p)}),
        RecordKind::Arc {
            layer,
            center,
            radius,
            start,
            sweep,
            properties: p,
        } => {
            json!({"kind":"arc","layer":layer,"center":point(center),"radius":radius,"start":start,"sweep":sweep,"properties":properties(p)})
        }
        RecordKind::Polygon {
            layer,
            points,
            properties: p,
        } => {
            json!({"kind":"polygon","layer":layer,"points":points.iter().map(point).collect::<Vec<_>>(),"properties":properties(p)})
        }
        RecordKind::Text {
            text,
            at,
            rotation,
            flip,
            scale,
            properties: p,
        } => {
            json!({"kind":"text","text":text,"at":point(at),"rotation":rotation,"flip":flip,"scale":point(scale),"properties":properties(p)})
        }
        RecordKind::Instance {
            symbol,
            at,
            rotation,
            flip,
            properties: p,
        } => {
            json!({"kind":"instance","symbol":symbol,"at":point(at),"rotation":rotation,"flip":flip,"properties":properties(p)})
        }
        RecordKind::Embedded(records) => {
            json!({"kind":"embedded","records":records.iter().map(|r|record(doc,r)).collect::<Vec<_>>()})
        }
        RecordKind::Comment => json!({"kind":"comment"}),
        RecordKind::Unknown { tag } => json!({"kind":"unknown","tag":tag}),
    };
    v["id"] = json!(format!("record:{}", r.span.start));
    v["line"] = json!(r.line);
    v["span"] = json!([r.span.start, r.span.end]);
    v["raw"] = json!(doc.raw_record(r));
    v
}
struct Resolver {
    library: Library,
    instances: Vec<Value>,
    sheets: Vec<Value>,
}
impl Resolver {
    fn sheet(
        &mut self,
        doc: &Document,
        source: &Path,
        path: &str,
        ports: Option<&[String]>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("resolved hierarchy exceeds 32 levels".into());
        }
        let sheet = resolve_sheet(&mut self.library, doc, source, ports, None)?;
        let sheet_id = if path.is_empty() { "/" } else { path };
        let nets = sheet
            .pin_names
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        self.sheets.push(json!({"id":sheet_id,"source":source,"ports":ports.unwrap_or(&[]),"nets":nets,
            "native_preambles":(['S','E'].iter().filter_map(|tag|doc.global(*tag).map(|p|json!({"tag":tag,"raw":p.raw}))).collect::<Vec<_>>())}));
        let mut names = HashSet::new();
        for c in &sheet.components {
            let name = c
                .value("name")
                .ok_or_else(|| format!("{}:{} missing instance name", source.display(), c.line))?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(format!("{path}: duplicate instance name {name}"));
            }
            let instance = format!("{path}/{name}");
            self.instances.push(json!({"id":instance,"sheet":sheet_id,"source":source,"line":c.line,
                "symbol":c.info.symbol,"resolved_symbol":c.info.resolved,"kind":c.kind(),
                "properties":properties(&c.info.properties),"symbol_defaults":properties(&c.defaults),"symbol_metadata":properties(&c.metadata),
                "pins":c.pins.iter().enumerate().map(|(order,(i,p))|json!({"order":order,"name":p.get("name"),"net":sheet.pin_names[*i],"properties":properties(p)})).collect::<Vec<_>>()}));
            if c.kind() == "subcircuit" {
                if c.flag("spice_stop") || c.flag("vacask_stop") {
                    return Err(format!(
                        "{instance}: opaque/stopped hierarchy has no complete resolved IR"
                    ));
                }
                let ports = c
                    .pins
                    .iter()
                    .map(|(_, p)| {
                        p.get("name")
                            .map(str::to_owned)
                            .ok_or("unnamed hierarchy pin")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let DocumentTarget::File(target) =
                    navigation::descend(&c.info, Descent::Schematic, &self.library.paths)?
                else {
                    unreachable!()
                };
                let child = self.library.document(&target)?;
                self.sheet(&child, &target, &instance, Some(&ports), depth + 1)?;
            }
        }
        Ok(())
    }
}

/// Snapshot the exact compiled records; also used for prepared-run provenance.
pub fn lowered_snapshot(circuit: &portable::Circuit, source: &Path, backend: &str) -> Value {
    json!({"schema_version":1,"stage":"lowered","source":source,"backend":backend,
        "devices":circuit.devices,"scopes":circuit.scopes,"signal_map":circuit.signals,
        "requires_ihp_lv_wrappers":circuit.ihp,"hierarchy_policy":"per-instance scalar specialization and flattening"})
}
