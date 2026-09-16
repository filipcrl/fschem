//! Read-only consumer of the Python runner's run.json v1. Never launches code.
use serde::Deserialize;
use std::{collections::BTreeMap, path::{Component, Path, PathBuf}};

#[derive(Debug, Deserialize)]
pub struct Manifest {
    schema_version: u64,
    pub status: String,
    pub backend: String,
    pub mode: String,
    pub source: PathBuf,
    pub analyses: serde_json::Value,
    artifacts: BTreeMap<String, serde_json::Value>,
    pub results: Vec<RunResult>,
    pub diagnostics: Vec<serde_json::Value>,
}
#[derive(Debug, Deserialize)]
pub struct RunResult {
    pub id: String,
    pub name: String,
    pub analysis_name: String,
    pub path: String,
    pub plot_index: usize,
    pub signals: Vec<String>,
    pub signal_map: BTreeMap<String, String>,
}
#[derive(Debug)]
pub struct RunArtifact {
    pub path: PathBuf,
    pub root: PathBuf,
    pub manifest: Manifest,
    pub files: Vec<(String, PathBuf)>,
}
impl RunArtifact {
    pub fn open(path: &Path) -> Result<Self, String> {
        let path = if path.is_dir() { path.join("run.json") } else { path.to_owned() };
        let path = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
        let root = path.parent().ok_or("manifest has no parent")?.to_owned();
        // Bound manifest memory, including malicious/unintentionally huge JSON.
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&path).map_err(|e| e.to_string())?.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 { return Err("run manifest exceeds 4 MiB".into()); }
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("invalid run manifest: {e}"))?;
        if manifest.schema_version != 1 { return Err("unsupported run manifest version".into()); }
        if !["prepared", "running", "succeeded", "failed", "cancelled", "timed_out"].contains(&manifest.status.as_str()) { return Err("invalid run status".into()); }
        if !["ngspice", "vacask"].contains(&manifest.backend.as_str()) || !["native", "portable"].contains(&manifest.mode.as_str()) { return Err("invalid run backend/mode".into()); }
        if !manifest.source.is_absolute() || !manifest.analyses.is_object() { return Err("invalid source/analyses".into()); }
        if manifest.status != "succeeded" && !manifest.results.is_empty() { return Err("partial run must not publish results".into()); }
        let mut files = Vec::new();
        for key in ["deck", "original_deck", "source_snapshot", "log"] {
            let relative = manifest.artifacts.get(key).and_then(|v| v.as_str()).ok_or_else(|| format!("missing artifact {key}"))?;
            files.push((key.to_owned(), contained(&root, relative)?));
        }
        let inputs = manifest.artifacts.get("inputs").and_then(|v| v.as_array()).ok_or("invalid artifact inputs")?;
        for input in inputs { let value = input.as_str().ok_or("invalid input path")?; files.push((format!("input: {value}"), contained(&root, value)?)); }
        // Optional additive measurement file; not currently emitted by the runner.
        if let Some(value) = manifest.artifacts.get("measurements") {
            files.push(("measurements".into(), contained(&root, value.as_str().ok_or("invalid measurements path")?)?));
        }
        let mut ids = std::collections::BTreeSet::new();
        for result in &manifest.results {
            contained(&root, &result.path)?;
            if !ids.insert(&result.id) || result.id != format!("{}#{}", result.path, result.plot_index) || result.signal_map.values().any(|v| !result.signals.contains(v)) {
                return Err("invalid result identity or signal mapping".into());
            }
        }
        Ok(Self { path, root, manifest, files })
    }
}
/// Reject traversal and symlink escapes even when a descendant is missing.
pub fn contained(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty() || relative.components().any(|c| !matches!(c, Component::Normal(_))) { return Err("artifact path must be relative and contained".into()); }
    let path = root.join(relative);
    let mut ancestor = path.as_path();
    loop {
        match ancestor.canonicalize() {
            Ok(resolved) => { if !resolved.starts_with(root) { return Err("artifact symlink escapes run directory".into()); } break; }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A broken symlink is not a legitimate missing artifact.
                if std::fs::symlink_metadata(ancestor).is_ok() { return Err("broken artifact symlink".into()); }
                ancestor = ancestor.parent().ok_or("invalid artifact path")?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_manifest_and_partial_files() {
        let root = std::env::temp_dir().join(format!("fschem-run-view-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("run.json");
        let mut value = serde_json::json!({"schema_version":1,"status":"prepared","backend":"ngspice","mode":"native","source":"/missing/source.sch","analyses":{},"artifacts":{"deck":"circuit.spice","original_deck":"original.deck","source_snapshot":"source.sch","log":"simulator.log","inputs":[]},"results":[],"diagnostics":[]});
        let write = |value: &serde_json::Value| std::fs::write(&path, value.to_string()).unwrap();
        write(&value);
        let run = RunArtifact::open(&root).unwrap();
        assert!(!run.files[0].1.exists());
        value["schema_version"] = 2.into(); write(&value); assert!(RunArtifact::open(&path).is_err());
        value["schema_version"] = 1.into(); value["status"] = "unknown".into(); write(&value); assert!(RunArtifact::open(&path).is_err());
        value["status"] = "failed".into(); value["artifacts"]["deck"] = "../outside".into(); write(&value); assert!(RunArtifact::open(&path).is_err());
        assert!(contained(&root, "/absolute").is_err());
        #[cfg(unix)] { std::os::unix::fs::symlink(std::env::temp_dir(), root.join("escape")).unwrap(); assert!(contained(&root, "escape/missing").is_err()); }
        std::fs::remove_dir_all(root).unwrap();
    }
}
