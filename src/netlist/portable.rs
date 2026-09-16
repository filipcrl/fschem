//! Portable lowering of resolved schematic connectivity, never generated decks.
use super::expression::Expr;
use super::*;
use std::collections::{BTreeMap, BTreeSet};
#[derive(serde::Serialize, Clone)]
pub struct Terminal {
    pub name: String,
    pub node: String,
}
#[derive(serde::Serialize)]
pub struct Device {
    pub id: String,
    pub source: PathBuf,
    pub line: Option<usize>,
    pub symbol: Option<String>,
    pub kind: String,
    pub model: String,
    pub emitted_name: String,
    pub terminals: Vec<Terminal>,
    pub expressions: BTreeMap<String, String>,
    pub parameters: BTreeMap<String, f64>,
    pub waveform: Option<Waveform>,
    pub behavior: Option<Expr>,
}
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Waveform {
    Dc {
        value: f64,
    },
    Pulse {
        low: f64,
        high: f64,
        delay: f64,
        rise: f64,
        fall: f64,
        width: f64,
        period: f64,
    },
    Pwl {
        points: Vec<[f64; 2]>,
    },
}
impl Waveform {
    fn emit(&self, vacask: bool) -> String {
        match self {
            Self::Dc { value } => {
                if vacask {
                    format!("dc={value:e}")
                } else {
                    format!("DC {value:e}")
                }
            }
            Self::Pulse {
                low,
                high,
                delay,
                rise,
                fall,
                width,
                period,
            } => {
                let args = [*low, *high, *delay, *rise, *fall, *width, *period];
                if vacask {
                    format!(
                        "type=\"pulse\" {}",
                        ["val0", "val1", "delay", "rise", "fall", "width", "period"]
                            .iter()
                            .zip(args)
                            .map(|(k, v)| format!("{k}={v:e}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                } else {
                    format!(
                        "PULSE({})",
                        args.iter()
                            .map(|v| format!("{v:e}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                }
            }
            Self::Pwl { points } => {
                let values = points
                    .iter()
                    .flatten()
                    .map(|v| format!("{v:e}"))
                    .collect::<Vec<_>>();
                if vacask {
                    format!("type=\"pwl\" wave=[{}]", values.join(", "))
                } else {
                    format!("PWL({})", values.join(" "))
                }
            }
        }
    }
}
/// The same elaborated records drive inspection and deck serialization.
fn emit(devices: &[Device], vacask: bool) -> String {
    let mut models = BTreeSet::new();
    let mut body = String::new();
    for d in devices {
        let nodes = d
            .terminals
            .iter()
            .map(|t| t.node.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let name = &d.emitted_name;
        if let Some(expr) = &d.behavior {
            let value = emit_behavior(expr);
            append(
                &mut body,
                &if vacask {
                    format!("{name} ({nodes}) v={value}")
                } else {
                    format!("{name} {nodes} V={value}")
                },
            );
            continue;
        }
        let wrapper = name.starts_with('x');
        let params = if let Some(w) = &d.waveform {
            w.emit(vacask)
        } else if !vacask && !wrapper {
            format!("{:e}", d.parameters.values().next().unwrap())
        } else {
            d.parameters
                .iter()
                .map(|(k, v)| format!("{k}={v:e}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        if vacask {
            let model = if wrapper {
                d.model.clone()
            } else {
                let model = &d.model;
                models.insert(format!(
                    "{}model fschem_{model} {model}",
                    if matches!(model.as_str(), "resistor" | "capacitor" | "inductor") {
                        format!("load \"{model}.osdi\"\n")
                    } else {
                        String::new()
                    }
                ));
                format!("fschem_{model}")
            };
            append(&mut body, &format!("{name} ({nodes}) {model} {params}"));
        } else {
            append(
                &mut body,
                &format!(
                    "{name} {nodes}{}{params}",
                    if wrapper {
                        format!(" {} ", d.model)
                    } else {
                        " ".into()
                    }
                ),
            );
        }
    }
    format!(
        "{}{body}",
        models
            .into_iter()
            .map(|m| format!("{m}\n"))
            .collect::<String>()
    )
}

#[derive(Default)]
pub struct Options {
    pub parameters: BTreeMap<String, String>,
    /// Exact instance paths; '/' is the root. Locals cannot redefine interfaces.
    pub local_parameters: BTreeMap<String, BTreeMap<String, String>>,
    /// Exact /instance/code paths or absolute-source.sch#code identifiers.
    pub exclude: BTreeSet<String>,
    pub sources: BTreeMap<String, (String, Vec<String>, String)>,
}
pub struct Circuit {
    pub deck: String,
    pub ihp: bool,
    pub signals: BTreeMap<String, String>,
    pub devices: Vec<Device>,
    pub scopes: Vec<serde_json::Value>,
}
pub fn compile(
    doc: &Document,
    source: &Path,
    paths: LibraryPaths,
    backend: &str,
    options: Options,
) -> Result<Circuit, String> {
    let mut circuit = elaborate(doc, source, paths, backend, options)?;
    circuit.deck = emit(&circuit.devices, backend == "vacask");
    Ok(circuit)
}
pub fn elaborate(
    doc: &Document,
    source: &Path,
    paths: LibraryPaths,
    backend: &str,
    options: Options,
) -> Result<Circuit, String> {
    if !matches!(backend, "ngspice" | "vacask") {
        return Err("unknown portable backend".into());
    }
    let mut compiler = Compiler {
        library: Library::new(paths),
        options,
        used_exclusions: BTreeSet::new(),
        used_scopes: BTreeSet::new(),
        devices: Vec::new(),
        scopes: Vec::new(),
        ihp: false,
        signals: BTreeMap::new(),
    };
    let scope = resolve_parameters(&compiler.options.parameters, &BTreeMap::new())?;
    compiler.sheet(
        doc,
        source,
        "",
        None,
        &BTreeMap::new(),
        scope,
        &BTreeSet::new(),
        0,
    )?;
    for key in compiler
        .options
        .exclude
        .difference(&compiler.used_exclusions)
    {
        return Err(format!("exclusion matched no native block: {key}"));
    }
    for key in compiler.options.local_parameters.keys() {
        if !compiler.used_scopes.contains(key) {
            return Err(format!("unknown local parameter scope {key}"));
        }
    }
    compiler.devices.sort_by(|a, b| a.id.cmp(&b.id));
    compiler
        .scopes
        .sort_by_key(|s| s["id"].as_str().unwrap().to_owned());
    let deck = String::new();
    Ok(Circuit {
        deck,
        ihp: compiler.ihp,
        signals: compiler.signals,
        devices: compiler.devices,
        scopes: compiler.scopes,
    })
}
fn identifier(value: &str) -> Result<&str, String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err(format!("unsupported portable identifier {value:?}"));
    }
    Ok(value)
}
fn eval(text: &str, scope: &BTreeMap<String, f64>) -> Result<f64, String> {
    Expr::parse(text)?.evaluate(scope)
}
fn resolve_parameters(
    defs: &BTreeMap<String, String>,
    base: &BTreeMap<String, f64>,
) -> Result<BTreeMap<String, f64>, String> {
    let mut scope = base.clone();
    for key in defs.keys() {
        identifier(key)?;
        scope.remove(key);
    }
    let mut pending = defs.clone();
    while !pending.is_empty() {
        let mut resolved = Vec::new();
        for (key, value) in &pending {
            let expr = Expr::parse(value)?;
            if let Ok(v) = expr.evaluate(&scope) {
                scope.insert(key.clone(), v);
                resolved.push(key.clone());
            }
        }
        if resolved.is_empty() {
            return Err(format!(
                "undefined, cyclic or invalid scoped expressions: {pending:?}"
            ));
        }
        for key in resolved {
            pending.remove(&key);
        }
    }
    Ok(scope)
}
fn encoded(path: &str) -> String {
    path.as_bytes().iter().map(|c| format!("{c:02x}")).collect()
}
struct Compiler {
    library: Library,
    options: Options,
    used_exclusions: BTreeSet<String>,
    used_scopes: BTreeSet<String>,
    devices: Vec<Device>,
    scopes: Vec<serde_json::Value>,
    ihp: bool,
    signals: BTreeMap<String, String>,
}
impl Compiler {
    #[allow(clippy::too_many_arguments)]
    fn sheet(
        &mut self,
        doc: &Document,
        source: &Path,
        path: &str,
        ports: Option<&[String]>,
        connections: &BTreeMap<String, String>,
        mut scope: BTreeMap<String, f64>,
        interface: &BTreeSet<String>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("portable hierarchy exceeds 32 levels".into());
        }
        for key in ['S', 'E'] {
            if doc.global(key).is_some_and(|p| !p.raw.trim().is_empty()) {
                return Err(format!(
                    "{}: native {key} preamble conflicts with portable mode; migrate it explicitly",
                    source.display()
                ));
            }
        }
        let scope_path = if path.is_empty() { "/" } else { path };
        if let Some(locals) = self.options.local_parameters.get(scope_path) {
            if locals
                .keys()
                .any(|k| interface.contains(k) || scope.contains_key(k))
            {
                return Err(format!(
                    "{scope_path}: local parameters shadow interface or inherited parameters"
                ));
            }
            scope = resolve_parameters(locals, &scope)?;
            self.used_scopes.insert(scope_path.into());
        }
        self.scopes.push(serde_json::json!({"id":scope_path,"source":source,"effective_parameters":scope,
            "interface_parameters":interface,"local_definitions":self.options.local_parameters.get(scope_path),
            "experiment_definitions":if path.is_empty(){Some(&self.options.parameters)}else{None}}));
        let sheet = resolve_sheet(&mut self.library, doc, source, ports, None)?;
        let mut node_map = BTreeMap::from([("0".into(), "0".into())]);
        for net in &sheet.pin_names {
            identifier(net)?;
            let node = if net == "0" {
                "0".into()
            } else if let Some(n) = connections.get(&net.to_ascii_lowercase()) {
                n.clone()
            } else if path.is_empty() {
                net.clone()
            } else {
                format!("n{}", encoded(&format!("{path}/{net}")))
            };
            node_map.insert(net.to_ascii_lowercase(), node.clone());
            self.signals.insert(format!("{path}/{net}"), node);
        }
        if path.is_empty() {
            for (key, (_, nodes, _)) in &self.options.sources {
                root_name(key)?;
                for node in nodes {
                    let node = root_name(node)?;
                    if !node_map.contains_key(&node.to_ascii_lowercase()) {
                        node_map.insert(node.to_ascii_lowercase(), node.into());
                    }
                }
            }
            for (_, (_, nodes, _)) in &self.options.sources {
                for node in nodes {
                    self.signals.insert(
                        node.clone(),
                        node_map[&root_name(node)?.to_ascii_lowercase()].clone(),
                    );
                }
            }
        }
        let mut names = BTreeSet::new();
        for c in &sheet.components {
            if is_label(c.kind()) || c.kind() == "noconn" {
                continue;
            }
            let name = identifier(c.value("name").ok_or("missing instance name")?)?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(format!("duplicate instance {path}/{name}"));
            }
            let instance = format!("{path}/{name}");
            if c.kind() == "netlist_commands" {
                let file_key = format!("{}#{name}", source.display());
                let mut excluded = false;
                for key in [&instance, &file_key] {
                    if self.options.exclude.contains(key) {
                        self.used_exclusions.insert(key.clone());
                        excluded = true;
                    }
                }
                if excluded {
                    continue;
                }
                return Err(format!(
                    "native code conflict at {instance} ({file_key}); exclude this exact block and migrate its parameters/analyses"
                ));
            }
            for flag in ["spice_short", "spice_ignore", "vacask_ignore"] {
                if c.flag(flag) {
                    return Err(format!(
                        "{instance}: backend-specific {flag} is unsupported in portable mode"
                    ));
                }
            }
            let mut nodes = Vec::new();
            for (i, _) in &c.pins {
                let net = &sheet.pin_names[*i];
                identifier(net)?;
                let node = if net == "0" {
                    "0".into()
                } else if let Some(node) = connections.get(&net.to_ascii_lowercase()) {
                    node.clone()
                } else if path.is_empty() {
                    net.clone()
                } else {
                    format!("n{}", encoded(&format!("{path}/{net}")))
                };
                self.signals.insert(format!("{path}/{net}"), node.clone());
                nodes.push(node);
            }
            let mut allowed: BTreeSet<&str> = [
                "name",
                "footprint",
                "device",
                "lab",
                "hide",
                "hide_texts",
                "highlight",
                "format",
                "spectre_format",
                "vacask_format",
                "vacask_params",
                "vacask_model",
                "only_toplevel",
                "savecurrent",
                "spice_ignore",
                "vacask_ignore",
                "spice_short",
                "spice_stop",
                "vacask_stop",
                "device_model",
            ]
            .into_iter()
            .collect();
            let fields: &[&str] = match c.kind() {
                "subcircuit" => &[],
                "nmos" | "pmos" => &["w", "l", "ng", "m", "mm_ok", "model", "spiceprefix"],
                "source" => &["VAR", "FUNC", "m"],
                "resistor" | "capacitor" | "inductor" => &["value", "m"],
                _ => &["value"],
            };
            allowed.extend(fields.iter().copied());
            if c.kind() == "subcircuit" {
                allowed.extend(c.defaults.entries.iter().map(|(k, _)| k.as_str()));
            }
            for (key, _) in c
                .info
                .properties
                .entries
                .iter()
                .chain(c.defaults.entries.iter())
            {
                if !allowed.contains(key.as_str()) {
                    return Err(format!("{instance}: unsupported property {key}"));
                }
            }
            let result = if c.kind() == "subcircuit" {
                if c.flag("spice_stop") || c.flag("vacask_stop") {
                    return Err(format!("{instance}: stopped/opaque hierarchy unsupported"));
                }
                let port_names = c
                    .pins
                    .iter()
                    .map(|(_, p)| {
                        p.get("name")
                            .map(str::to_owned)
                            .ok_or("unnamed subcircuit pin")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let connections = port_names
                    .iter()
                    .zip(nodes)
                    .map(|(p, n)| (p.to_ascii_lowercase(), n))
                    .collect();
                let mut defaults = BTreeMap::new();
                let mut overrides = BTreeMap::new();
                for (key, value) in &c.defaults.entries {
                    if key == "name" {
                        continue;
                    }
                    let value = value.as_ref().ok_or("empty interface default")?;
                    defaults.insert(key.clone(), value.clone());
                    if let Some(value) = c.info.properties.get(key) {
                        overrides.insert(key.clone(), eval(value, &scope)?);
                    }
                }
                let interface_defaults = defaults.clone();
                let instance_overrides = defaults
                    .keys()
                    .filter_map(|k| c.info.properties.get(k).map(|v| (k.clone(), v.to_owned())))
                    .collect::<BTreeMap<_, _>>();
                // Defaults are defined in their own scope; instance expressions use the caller scope.
                for (key, value) in overrides {
                    defaults.insert(key, format!("{value:e}"));
                }
                let child = resolve_parameters(&defaults, &BTreeMap::new())?;
                let interface = defaults.keys().cloned().collect();
                let DocumentTarget::File(target) =
                    navigation::descend(&c.info, Descent::Schematic, &self.library.paths)?
                else {
                    unreachable!()
                };
                let child_doc = self.library.document(&target)?;
                self.sheet(
                    &child_doc,
                    &target,
                    &instance,
                    Some(&port_names),
                    &connections,
                    child,
                    &interface,
                    depth + 1,
                )?;
                let scope = self
                    .scopes
                    .iter_mut()
                    .find(|v| v["id"] == instance)
                    .unwrap();
                scope["interface_defaults"] = serde_json::json!(interface_defaults);
                scope["instance_overrides"] = serde_json::json!(instance_overrides);
                Ok(())
            } else {
                self.device(c, &nodes, &instance, &scope, &node_map)
            };
            result.map_err(|e| format!("{instance}: {e}"))?;
        }
        if path.is_empty() {
            for (key, (kind, nodes, value)) in &self.options.sources {
                let name = root_name(key)?;
                if !names.insert(name.to_ascii_lowercase()) {
                    return Err(format!("supplemental source name collision: {key}"));
                }
                if nodes.len() != 2 {
                    return Err("supplemental source requires exactly two terminals".into());
                }
                let prefix = match kind.as_str() {
                    "voltage" => 'v',
                    "current" => 'i',
                    _ => return Err(format!("unsupported supplemental source kind {kind}")),
                };
                let nodes = nodes
                    .iter()
                    .map(|n| root_name(n).map(|n| node_map[&n.to_ascii_lowercase()].clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                if nodes[0] == nodes[1] {
                    return Err("supplemental source terminals must differ".into());
                }
                let model = if prefix == 'v' { "vsource" } else { "isource" };
                self.devices.push(Device {
                    id: key.clone(),
                    source: source.into(),
                    line: None,
                    symbol: None,
                    kind: model.into(),
                    model: model.into(),
                    emitted_name: format!("{prefix}{}", encoded(key)),
                    terminals: ["P", "N"]
                        .iter()
                        .zip(nodes)
                        .map(|(p, n)| Terminal {
                            name: (*p).into(),
                            node: n,
                        })
                        .collect(),
                    parameters: BTreeMap::new(),
                    expressions: BTreeMap::from([("value".into(), value.clone())]),
                    waveform: Some(waveform(value, &scope)?),
                    behavior: None,
                });
            }
        }
        Ok(())
    }
    fn device(
        &mut self,
        c: &Component,
        nodes: &[String],
        path: &str,
        scope: &BTreeMap<String, f64>,
        node_map: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let value = |key: &str| eval(c.value(key).ok_or_else(|| format!("missing {key}"))?, scope);
        let mut device = Device {
            id: path.into(),
            source: c.info.source_file.clone(),
            line: Some(c.line),
            symbol: Some(c.info.symbol.clone()),
            kind: c.kind().into(),
            model: String::new(),
            emitted_name: String::new(),
            terminals: c
                .pins
                .iter()
                .zip(nodes)
                .map(|((_, p), n)| Terminal {
                    name: p.get("name").unwrap_or("").into(),
                    node: n.clone(),
                })
                .collect(),
            parameters: BTreeMap::new(),
            expressions: BTreeMap::new(),
            waveform: None,
            behavior: None,
        };
        for key in ["value", "w", "l", "ng", "m", "mm_ok", "FUNC"] {
            if let Some(text) = c.value(key) {
                device.expressions.insert(key.into(), text.into());
            }
        }
        let sym = c.symname();
        let ihp = matches!(sym.as_str(), "sg13_lv_nmos" | "sg13_lv_pmos")
            && matches!(c.kind(), "nmos" | "pmos");
        let prefix = if ihp {
            if c.value("spiceprefix")
                .is_some_and(|s| !s.eq_ignore_ascii_case("x"))
            {
                return Err("IHP requires subcircuit wrapper prefix X".into());
            }
            if value("ng")?.fract() != 0. {
                return Err("IHP ng must be an integer".into());
            }
            if c.value("model") != Some(sym.as_str()) {
                return Err("IHP symbol/model mismatch".into());
            }
            if nodes.len() != 4 {
                return Err("IHP requires four terminals".into());
            }
            let mut ordered = Vec::new();
            for pin in ["D", "G", "S", "B"] {
                let found = device
                    .terminals
                    .iter()
                    .filter(|t| t.name.eq_ignore_ascii_case(pin))
                    .collect::<Vec<_>>();
                if found.len() != 1 {
                    return Err(format!("IHP requires unique named {pin} terminal"));
                }
                ordered.push(Terminal {
                    name: pin.into(),
                    node: found[0].node.clone(),
                });
            }
            device.terminals = ordered;
            for key in ["w", "l", "ng", "m", "mm_ok"] {
                let v = value(key)?;
                if (key == "mm_ok" && ![0., 1.].contains(&v)) || (key != "mm_ok" && v <= 0.) {
                    return Err(format!("invalid IHP {key}"));
                }
                device.parameters.insert(key.into(), v);
            }
            self.ihp = true;
            device.model = sym;
            'x'
        } else if c.kind() == "source" {
            if c.value("VAR") != Some("V")
                || nodes.len() != 2
                || c.value("m")
                    .map(|s| eval(s, scope))
                    .transpose()?
                    .unwrap_or(1.)
                    != 1.
            {
                return Err(
                    "only unity-multiplicity behavioral voltage sources are supported".into(),
                );
            }
            device.behavior = Some(lower_behavior(
                &Expr::parse(c.value("FUNC").ok_or("missing behavioral FUNC")?)?,
                scope,
                node_map,
            )?);
            device.model = "behavioral_voltage".into();
            'b'
        } else {
            let (prefix, model, parameter) = match c.kind() {
                "resistor" => ('r', "resistor", "r"),
                "capacitor" => ('c', "capacitor", "c"),
                "inductor" => ('l', "inductor", "l"),
                "vsource" => ('v', "vsource", "dc"),
                "isource" => ('i', "isource", "dc"),
                "vcvs" => ('e', "vcvs", "gain"),
                _ => {
                    return Err(format!(
                        "unsupported portable device {:?} ({sym})",
                        c.kind()
                    ));
                }
            };
            if nodes.len() != if prefix == 'e' { 4 } else { 2 } {
                return Err("incorrect terminal count".into());
            }
            if c.value("device_model").is_some_and(|v| !v.is_empty()) {
                return Err("custom device_model requires an explicit technology binding".into());
            }
            if matches!(prefix, 'v' | 'i') {
                device.waveform = Some(waveform(
                    c.value("value").ok_or("missing source value")?,
                    scope,
                )?);
            } else {
                let mut v = value("value")?;
                if prefix != 'e' {
                    let m = c
                        .value("m")
                        .map(|s| eval(s, scope))
                        .transpose()?
                        .unwrap_or(1.);
                    if m <= 0. || v <= 0. {
                        return Err("passive value/multiplicity must be positive".into());
                    }
                    v = if prefix == 'c' { v * m } else { v / m };
                }
                if !v.is_finite() {
                    return Err("non-finite effective device value".into());
                }
                device.parameters.insert(parameter.into(), v);
            }
            device.model = model.into();
            prefix
        };
        device.emitted_name = format!("{prefix}{}", encoded(path));
        self.devices.push(device);
        Ok(())
    }
}
fn root_name(path: &str) -> Result<&str, String> {
    let name = path
        .strip_prefix('/')
        .ok_or("supplemental paths must start with /")?;
    identifier(name).map_err(|_| format!("supplemental sources require root-only paths: {path}"))
}
fn lower_behavior(
    expr: &Expr,
    scope: &BTreeMap<String, f64>,
    nodes: &BTreeMap<String, String>,
) -> Result<Expr, String> {
    Ok(match expr {
        Expr::Voltage(a, b) => {
            let node = |s: &str| {
                nodes
                    .get(&s.to_ascii_lowercase())
                    .cloned()
                    .ok_or_else(|| format!("undefined behavioral voltage node {s}"))
            };
            Expr::Voltage(node(a)?, b.as_ref().map(|b| node(b)).transpose()?)
        }
        Expr::Unary(op, v) => Expr::Unary(*op, Box::new(lower_behavior(v, scope, nodes)?)),
        Expr::Binary(op, a, b) => {
            if *op == '^' {
                return Err("behavioral powers unsupported".into());
            }
            Expr::Binary(
                *op,
                Box::new(lower_behavior(a, scope, nodes)?),
                Box::new(lower_behavior(b, scope, nodes)?),
            )
        }
        _ => Expr::Number(expr.evaluate(scope)?),
    })
}
fn emit_behavior(expr: &Expr) -> String {
    match expr {
        Expr::Voltage(a, b) => {
            if let Some(b) = b {
                format!("v({a},{b})")
            } else {
                format!("v({a})")
            }
        }
        Expr::Unary(op, v) => format!("({op}{})", emit_behavior(v)),
        Expr::Binary(op, a, b) => format!("({} {op} {})", emit_behavior(a), emit_behavior(b)),
        Expr::Number(v) => format!("{v:e}"),
        Expr::Name(_) => unreachable!("lowered expressions contain no unresolved parameters"),
    }
}
fn waveform(text: &str, scope: &BTreeMap<String, f64>) -> Result<Waveform, String> {
    let text = text.trim();
    let upper = text.to_ascii_uppercase();
    for kind in ["PULSE", "PWL"] {
        if upper.starts_with(kind) {
            let content = text[kind.len()..]
                .trim()
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .ok_or("waveform requires parentheses")?;
            let mut parts = Vec::new();
            let mut start = 0;
            let mut depth = 0i32;
            for (i, ch) in content.char_indices() {
                match ch {
                    '{' | '(' => depth += 1,
                    '}' | ')' => depth -= 1,
                    _ => {}
                }
                if depth < 0 {
                    return Err("unbalanced waveform expression".into());
                }
                if depth == 0 && (ch.is_whitespace() || ch == ',') {
                    if start < i {
                        parts.push(eval(&content[start..i], scope)?);
                    }
                    start = i + ch.len_utf8();
                }
            }
            if depth != 0 {
                return Err("unbalanced waveform expression".into());
            }
            if start < content.len() {
                parts.push(eval(&content[start..], scope)?);
            }
            if kind == "PULSE" {
                if parts.len() != 7 {
                    return Err("PULSE requires exactly 7 arguments".into());
                }
                if parts[2..].iter().any(|v| *v < 0.) || parts[6] <= 0. {
                    return Err("invalid PULSE timing".into());
                }
                return Ok(Waveform::Pulse {
                    low: parts[0],
                    high: parts[1],
                    delay: parts[2],
                    rise: parts[3],
                    fall: parts[4],
                    width: parts[5],
                    period: parts[6],
                });
            } else {
                if parts.len() < 4 || parts.len() % 2 != 0 {
                    return Err("PWL requires time/value pairs".into());
                }
                let times = parts.iter().step_by(2).copied().collect::<Vec<_>>();
                if times[0] < 0. || times.windows(2).any(|w| w[1] <= w[0]) {
                    return Err("PWL times must strictly increase".into());
                }
                return Ok(Waveform::Pwl {
                    points: parts.chunks_exact(2).map(|p| [p[0], p[1]]).collect(),
                });
            }
        }
    }
    let text = if upper.starts_with("DC ") {
        text[3..].trim()
    } else {
        text
    };
    let v = eval(text, scope)?;
    Ok(Waveform::Dc { value: v })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waveforms() {
        let scope = BTreeMap::from([("T".into(), 1e-9)]);
        assert!(
            waveform("PULSE(0 1 {2*T} 1p 1p {T} {4*T})", &scope)
                .unwrap()
                .emit(true)
                .contains("period=4e-9")
        );
        assert!(
            waveform("PWL(0 0 1n 1)", &scope)
                .unwrap()
                .emit(true)
                .contains("wave=[")
        );
        for s in ["SIN(0 1 1k)", "PWL(1 0 0 1)", "PULSE(0 1)"] {
            assert!(waveform(s, &scope).is_err());
        }
    }
    #[test]
    fn parameter_dependencies() {
        let defs = BTreeMap::from([("a".into(), "2*b".into()), ("b".into(), "3u".into())]);
        assert_eq!(
            resolve_parameters(&defs, &BTreeMap::new()).unwrap()["a"],
            6e-6
        );
        assert!(
            resolve_parameters(
                &BTreeMap::from([("a".into(), "a+1".into())]),
                &BTreeMap::new()
            )
            .is_err()
        );
    }
}
