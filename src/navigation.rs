//! Navigation targets and Xschem symbol-to-schematic resolution, independent of
//! the GUI. Each document tab owns a stack of views for all target types.
use crate::{
    document::{Document, Properties},
    library::LibraryPaths,
    scene::{InstanceInfo, LoadedDocument},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Descent {
    Symbol,
    Schematic,
}

#[derive(Clone, Debug)]
pub enum DocumentTarget {
    File(PathBuf),
    Embedded {
        document: Arc<Document>,
        source: PathBuf,
        symbol: String,
    },
}
impl DocumentTarget {
    pub fn load(&self, paths: LibraryPaths) -> Result<LoadedDocument, String> {
        match self {
            Self::File(path) => LoadedDocument::open(path, paths),
            Self::Embedded {
                document, source, ..
            } => {
                LoadedDocument::from_document_at(Arc::clone(document), None, source.clone(), paths)
            }
        }
    }
    pub fn name(&self) -> String {
        match self {
            Self::File(path) => path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            Self::Embedded { symbol, .. } => format!("{symbol} (embedded)"),
        }
    }
    pub fn description(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Embedded { source, symbol, .. } => {
                format!("{symbol} — embedded in {}", source.display())
            }
        }
    }
    pub fn directory(&self) -> &Path {
        match self {
            Self::File(path) | Self::Embedded { source: path, .. } => {
                path.parent().unwrap_or(Path::new("."))
            }
        }
    }
}

pub fn descend(
    instance: &InstanceInfo,
    kind: Descent,
    paths: &LibraryPaths,
) -> Result<DocumentTarget, String> {
    if kind == Descent::Symbol {
        if instance.embedded
            && let Some(document) = &instance.definition
        {
            return Ok(DocumentTarget::Embedded {
                document: Arc::clone(document),
                source: instance.source_file.clone(),
                symbol: instance.symbol.clone(),
            });
        }
        return instance
            .resolved
            .clone()
            .map(DocumentTarget::File)
            .ok_or_else(|| {
                format!(
                    "Cannot open unresolved symbol {:?}. Check the library paths.",
                    instance.symbol
                )
            });
    }

    let metadata = instance
        .definition
        .as_ref()
        .and_then(|doc| doc.symbol_properties());
    let owner = instance.source_file.parent().unwrap_or(Path::new("."));
    let symbol_dir = instance
        .resolved
        .as_ref()
        .and_then(|path| path.parent())
        .unwrap_or(owner);
    let override_from_instance = instance
        .properties
        .get("schematic")
        .filter(|value| !value.is_empty());
    let schematic = override_from_instance.or_else(|| {
        metadata
            .and_then(|p| p.get("schematic"))
            .filter(|value| !value.is_empty())
    });
    if let Some(reference) = schematic {
        let reference = expand_reference(reference, instance, metadata)?;
        let dirs = if override_from_instance.is_some() {
            [owner, symbol_dir]
        } else {
            [symbol_dir, owner]
        };
        return paths
            .resolve_schematic(&reference, &dirs)
            .map(DocumentTarget::File);
    }

    // Prefer the .sch beside the actual resolved .sym, not an unrelated
    // schematic of the same basename earlier in a different library.
    if let Some(symbol) = &instance.resolved {
        let schematic = symbol.with_extension("sch");
        if schematic.is_file() {
            return Ok(DocumentTarget::File(schematic));
        }
    }
    let schematic = Path::new(&instance.symbol).with_extension("sch");
    paths
        .resolve_schematic(&schematic.to_string_lossy(), &[owner])
        .map(DocumentTarget::File)
        .map_err(|error| format!("Cannot descend into {:?}: {error}", instance.symbol))
}

fn expand_reference(
    reference: &str,
    instance: &InstanceInfo,
    metadata: Option<&Properties>,
) -> Result<String, String> {
    let template = Properties::parse(
        metadata
            .and_then(|p| p.get("template"))
            .unwrap_or("")
            .into(),
    );
    let mut result = String::new();
    let mut chars = reference.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '@' {
            result.push(c);
            continue;
        }
        let mut key = String::new();
        while chars
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            key.push(chars.next().unwrap());
        }
        if key == "symname" {
            result.push_str(
                &Path::new(&instance.symbol)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy(),
            );
        } else if let Some(value) = instance.properties.get(&key).or_else(|| template.get(&key)) {
            result.push_str(value);
        } else {
            return Err(format!(
                "Unresolved attribute @{key} in schematic reference {reference:?}"
            ));
        }
    }
    if result.contains('@') || result.contains("tcleval(") {
        return Err(format!(
            "Schematic reference {reference:?} requires dynamic Xschem evaluation"
        ));
    }
    Ok(result)
}
