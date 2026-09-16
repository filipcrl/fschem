//! Portable lowering of resolved schematic connectivity, never generated decks.
use super::expression::Expr;
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct Options {
    pub parameters: BTreeMap<String, String>,
    /// Exact instance paths; '/' is the root. Locals cannot redefine interfaces.
    pub local_parameters: BTreeMap<String, BTreeMap<String, String>>,
    /// Exact /instance/code paths or absolute-source.sch#code identifiers.
    pub exclude: BTreeSet<String>,
}
pub struct Circuit {
    pub deck: String,
    pub ihp: bool,
    pub signals: BTreeMap<String, String>,
}
pub fn compile(
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
        models: BTreeSet::new(),
        body: String::new(),
        vacask: backend == "vacask",
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
    let deck = format!(
        "{}{}",
        compiler
            .models
            .into_iter()
            .map(|s| format!("{s}\n"))
            .collect::<String>(),
        compiler.body
    );
    Ok(Circuit {
        deck,
        ihp: compiler.ihp,
        signals: compiler.signals,
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
    models: BTreeSet<String>,
    body: String,
    vacask: bool,
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
        let sheet = resolve_sheet(&mut self.library, doc, source, ports, None)?;
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
                // Defaults are defined in their own scope; instance expressions use the caller scope.
                let mut child = resolve_parameters(&defaults, &BTreeMap::new())?;
                child.extend(overrides);
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
                )
            } else {
                self.device(c, &nodes, &instance, &scope)
            };
            result.map_err(|e| format!("{instance}: {e}"))?;
        }
        Ok(())
    }
    fn device(
        &mut self,
        c: &Component,
        nodes: &[String],
        path: &str,
        scope: &BTreeMap<String, f64>,
    ) -> Result<(), String> {
        let value = |key: &str| -> Result<f64, String> {
            eval(c.value(key).ok_or_else(|| format!("missing {key}"))?, scope)
        };
        let sym = c.symname();
        let ihp = matches!(sym.as_str(), "sg13_lv_nmos" | "sg13_lv_pmos")
            && matches!(c.kind(), "nmos" | "pmos");
        let (prefix, model, params, mut nodes) = if ihp {
            if c.value("model") != Some(sym.as_str()) {
                return Err("IHP symbol/model mismatch".into());
            }
            let mut ordered = Vec::new();
            for pin in ["D", "G", "S", "B"] {
                let indices = c
                    .pins
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, p))| {
                        p.get("name").is_some_and(|n| n.eq_ignore_ascii_case(pin))
                    })
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>();
                if indices.len() != 1 {
                    return Err(format!("IHP requires unique named {pin} terminal"));
                }
                ordered.push(nodes[indices[0]].clone());
            }
            if nodes.len() != 4 {
                return Err("IHP requires four terminals".into());
            }
            let mut params = Vec::new();
            for key in ["w", "l", "ng", "m", "mm_ok"] {
                let v = value(key)?;
                if (key == "mm_ok" && ![0., 1.].contains(&v)) || (key != "mm_ok" && v <= 0.) {
                    return Err(format!("invalid IHP {key}"));
                }
                params.push(format!("{key}={v:e}"));
            }
            self.ihp = true;
            ('x', sym, params.join(" "), ordered)
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
            let params = if prefix == 'v' || prefix == 'i' {
                source(
                    c.value("value").ok_or("missing source value")?,
                    scope,
                    self.vacask,
                )?
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
                if self.vacask {
                    format!("{parameter}={v:e}")
                } else {
                    format!("{v:e}")
                }
            };
            if self.vacask {
                self.models.insert(format!(
                    "{}model fschem_{model} {model}",
                    if matches!(prefix, 'r' | 'c' | 'l') {
                        format!("load \"{model}.osdi\"\n")
                    } else {
                        String::new()
                    }
                ));
            }
            (prefix, format!("fschem_{model}"), params, nodes.to_vec())
        };
        let name = format!("{prefix}{}", encoded(path));
        if self.vacask {
            append(
                &mut self.body,
                &format!("{name} ({}) {model} {params}", nodes.join(" ")),
            );
        } else {
            if ihp {
                nodes.push(model);
            }
            append(
                &mut self.body,
                &format!("{name} {} {params}", nodes.join(" ")),
            );
        }
        Ok(())
    }
}
fn source(text: &str, scope: &BTreeMap<String, f64>, vacask: bool) -> Result<String, String> {
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
                if vacask {
                    return Ok(format!(
                        "type=\"pulse\" {}",
                        ["val0", "val1", "delay", "rise", "fall", "width", "period"]
                            .iter()
                            .zip(parts)
                            .map(|(k, v)| format!("{k}={v:e}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
            } else {
                if parts.len() < 4 || parts.len() % 2 != 0 {
                    return Err("PWL requires time/value pairs".into());
                }
                let times = parts.iter().step_by(2).copied().collect::<Vec<_>>();
                if times[0] < 0. || times.windows(2).any(|w| w[1] <= w[0]) {
                    return Err("PWL times must strictly increase".into());
                }
                if vacask {
                    return Ok(format!(
                        "type=\"pwl\" wave=[{}]",
                        parts
                            .iter()
                            .map(|v| format!("{v:e}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            return Ok(format!(
                "{kind}({})",
                parts
                    .iter()
                    .map(|v| format!("{v:e}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    }
    let text = if upper.starts_with("DC ") {
        text[3..].trim()
    } else {
        text
    };
    let v = eval(text, scope)?;
    Ok(if vacask {
        format!("dc={v:e}")
    } else {
        format!("DC {v:e}")
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waveforms() {
        let scope = BTreeMap::from([("T".into(), 1e-9)]);
        assert!(
            source("PULSE(0 1 {2*T} 1p 1p {T} {4*T})", &scope, true)
                .unwrap()
                .contains("period=4e-9")
        );
        assert!(
            source("PWL(0 0 1n 1)", &scope, true)
                .unwrap()
                .contains("wave=[")
        );
        for s in ["SIN(0 1 1k)", "PWL(1 0 0 1)", "PULSE(0 1)"] {
            assert!(source(s, &scope, false).is_err());
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
