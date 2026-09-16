use crate::document::Document;
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

pub fn read_document(path: &Path) -> Result<Arc<Document>, String> {
    let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} exceeds the 128 MiB document limit",
            path.display()
        ));
    }
    // Do not decode lossily: an unchanged file must retain all its original bytes.
    let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Document::parse(source)
        .map(Arc::new)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[derive(Clone, Debug, Default)]
pub struct LibraryPaths {
    pub roots: Vec<PathBuf>,
    /// Project-local resolver variables, never exported to the process.
    pub variables: HashMap<String, String>,
}
impl LibraryPaths {
    pub fn discover(explicit: Vec<PathBuf>) -> Self {
        let mut roots = explicit;
        if let Some(paths) = env::var_os("XSCHEM_LIBRARY_PATH") {
            roots.extend(env::split_paths(&paths));
        }
        if let Some(share) = env::var_os("XSCHEM_SHAREDIR") {
            roots.push(PathBuf::from(share).join("xschem_library"));
        }
        if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
            roots.push(PathBuf::from(home).join(".xschem/xschem_library"));
        }
        if let Ok(cwd) = env::current_dir() {
            // Convenient development layout; no compile-time absolute paths.
            roots.extend([
                cwd.join("../xschem/xschem_library"),
                cwd.join("xschem/xschem_library"),
            ]);
        }
        for prefix in ["/usr/local", "/usr", "/opt/homebrew", "/opt/local"] {
            roots.push(PathBuf::from(prefix).join("share/xschem/xschem_library"));
        }
        let mut result = Self::default();
        for root in roots {
            // Keep explicit nonexistent roots in the UI so mistakes are visible.
            let root = fs::canonicalize(&root).unwrap_or(root);
            if !result.roots.contains(&root) {
                result.roots.push(root);
            }
        }
        result
    }
    pub fn resolve(
        &self,
        reference: &str,
        owner_dir: &Path,
        legacy: bool,
    ) -> Result<PathBuf, String> {
        self.resolve_document(reference, &[owner_dir], legacy.then_some("sym"), "symbol")
    }

    pub fn resolve_schematic(
        &self,
        reference: &str,
        owner_dirs: &[&Path],
    ) -> Result<PathBuf, String> {
        self.resolve_document(reference, owner_dirs, Some("sch"), "schematic")
    }

    fn resolve_document(
        &self,
        reference: &str,
        owner_dirs: &[&Path],
        default_extension: Option<&str>,
        kind: &str,
    ) -> Result<PathBuf, String> {
        let mut reference = expand_environment(reference, |name| self.variables.get(name).cloned().or_else(|| env::var(name).ok()))?;
        if reference.starts_with("~/") {
            let home = env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .ok_or("cannot expand ~ without a home directory")?;
            reference = PathBuf::from(home)
                .join(&reference[2..])
                .to_string_lossy()
                .into_owned();
        }
        if reference.contains("tcleval(")
            || Path::new(&reference)
                .extension()
                .is_some_and(|ext| ext == "tcl")
            || reference.contains(".tcl(")
            || reference.ends_with(')')
        {
            return Err(format!(
                "generated/Tcl {kind} {reference:?} requires Xschem evaluation"
            ));
        }
        if Path::new(&reference).extension().is_none()
            && let Some(extension) = default_extension
        {
            reference.push('.');
            reference.push_str(extension);
        }
        let relative = Path::new(&reference);
        if relative.is_absolute() {
            return fs::canonicalize(relative).map_err(|_| format!("missing {kind} {reference:?}"));
        }
        let candidates =
            owner_dirs
                .iter()
                .map(|dir| dir.join(relative))
                .chain(self.roots.iter().flat_map(|root| {
                    // Bare library symbols such as lab_pin.sym are common in old files.
                    [root.join(relative), root.join("devices").join(relative)]
                }));
        for candidate in candidates {
            if candidate.is_file() {
                return fs::canonicalize(&candidate).map_err(|e| e.to_string());
            }
        }
        Err(format!(
            "missing {kind} {reference:?} (searched the referring document directories and configured libraries)"
        ))
    }
}

/// Only environment expansion, not Tcl evaluation. Unknown variables produce
/// an actionable diagnostic instead of accidentally resolving another file.
pub fn expand_environment(
    input: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let mut result = String::new();
    let mut rest = input;
    while let Some(index) = rest.find('$') {
        result.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        let (name, consumed) = if let Some(after) = rest.strip_prefix("env(") {
            let end = after.find(')').ok_or("unterminated $env(...) reference")?;
            (&after[..end], end + 5)
        } else if let Some(after) = rest.strip_prefix('{') {
            let end = after.find('}').ok_or("unterminated ${...} reference")?;
            (&after[..end], end + 2)
        } else {
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            (&rest[..end], end)
        };
        if name.is_empty() {
            return Err("unresolved Tcl variable in symbol reference".into());
        }
        result.push_str(
            &lookup(name).ok_or_else(|| format!("unresolved environment/Tcl variable {name:?}"))?,
        );
        rest = &rest[consumed..];
    }
    result.push_str(rest);
    Ok(result)
}

pub struct Library {
    pub paths: LibraryPaths,
    cache: HashMap<PathBuf, Result<Arc<Document>, String>>,
}
impl Library {
    pub fn new(paths: LibraryPaths) -> Self {
        Self {
            paths,
            cache: HashMap::new(),
        }
    }
    pub fn document(&mut self, path: &Path) -> Result<Arc<Document>, String> {
        self.cache
            .entry(path.to_path_buf())
            .or_insert_with(|| read_document(path))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_environment_without_interpreting_tcl() {
        let value =
            expand_environment("$env(PDK)/${LIB}/$CELL.sym", |key| Some(key.to_lowercase()))
                .unwrap();
        assert_eq!(value, "pdk/lib/cell.sym");
        assert!(expand_environment("$env(UNKNOWN)/a.sym", |_| None).is_err());
        assert!(expand_environment("$env(unclosed", |_| None).is_err());
    }
}
