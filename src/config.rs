//! Declarative project configuration; loading never executes code or mutates the process environment.
use crate::library::{LibraryPaths, expand_environment};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

pub const FILE_NAME: &str = ".fschem.toml";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub schema_version: u32,
    pub library_paths: Vec<PathBuf>,
    pub pdk_root: Option<PathBuf>,
    pub pdk: Option<String>,
    pub simulator: Option<String>,
    #[serde(skip_deserializing)]
    pub path: Option<PathBuf>,
}
impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            library_paths: vec![],
            pdk_root: None,
            pdk: None,
            simulator: None,
            path: None,
        }
    }
}
impl ProjectConfig {
    /// Explicit configuration wins; otherwise discover the closest ancestor of
    /// a document or project directory. Never merge unrelated ancestor projects.
    pub fn load(start: &Path, explicit: Option<&Path>, disabled: bool) -> Result<Self, String> {
        if disabled {
            return Ok(Self::default());
        }
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        let start = if start.is_absolute() {
            start.to_owned()
        } else {
            cwd.join(start)
        };
        let dir = if start.is_dir() {
            start.as_path()
        } else {
            start.parent().unwrap_or(&cwd)
        };
        let path = if let Some(path) = explicit {
            Some(if path.is_absolute() {
                path.to_owned()
            } else {
                cwd.join(path)
            })
        } else {
            dir.ancestors()
                .map(|p| p.join(FILE_NAME))
                .find(|p| p.exists())
        };
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let metadata = fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !metadata.is_file() || metadata.len() > 65536 {
            return Err(format!(
                "{}: config must be a regular file at most 64 KiB",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut config: Self =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if config.schema_version != 1 {
            return Err(format!(
                "{}: unsupported schema_version {}",
                path.display(),
                config.schema_version
            ));
        }
        if config
            .simulator
            .as_deref()
            .is_some_and(|s| !matches!(s, "ngspice" | "vacask"))
        {
            return Err(format!(
                "{}: simulator must be ngspice or vacask",
                path.display()
            ));
        }
        if config
            .pdk
            .as_ref()
            .is_some_and(|s| s.is_empty() || matches!(s.as_str(), "." | "..") || s.contains(['/', '\\', '$']))
        {
            return Err(format!(
                "{}: pdk must be a nonempty directory name",
                path.display()
            ));
        }
        let base = path.parent().unwrap();
        if let Some(root) = &config.pdk_root {
            config.pdk_root = Some(
                resolve_path(root, base, &HashMap::new())
                    .map_err(|e| format!("{}: {e}", path.display()))?,
            );
        }
        let variables = config.variables();
        config.library_paths = config
            .library_paths
            .iter()
            .map(|p| resolve_path(p, base, &variables))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        config.path = Some(path);
        Ok(config)
    }
    pub fn variables(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        if let Some(root) = &self.pdk_root {
            vars.insert("PDK_ROOT".into(), root.to_string_lossy().into_owned());
        }
        if let Some(pdk) = &self.pdk {
            vars.insert("PDK".into(), pdk.clone());
        }
        vars
    }
    pub fn libraries(
        &self,
        explicit: Vec<PathBuf>,
        discover: bool,
    ) -> Result<LibraryPaths, String> {
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        let variables = self.variables();
        let mut roots = explicit
            .iter()
            .map(|p| resolve_path(p, &cwd, &variables))
            .collect::<Result<Vec<_>, _>>()?;
        roots.extend(self.library_paths.iter().cloned());
        let mut paths = if discover {
            LibraryPaths::discover(roots)
        } else {
            LibraryPaths {
                roots,
                ..Default::default()
            }
        };
        paths.variables = variables;
        Ok(paths)
    }
}
fn resolve_path(
    path: &Path,
    base: &Path,
    variables: &HashMap<String, String>,
) -> Result<PathBuf, String> {
    let value = expand_environment(&path.to_string_lossy(), |k| {
        variables.get(k).cloned().or_else(|| env::var(k).ok())
    })?;
    let expanded = if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .ok_or("cannot expand ~ without HOME")?,
        )
        .join(rest)
    } else {
        PathBuf::from(value)
    };
    let path = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    Ok(fs::canonicalize(&path).unwrap_or(path))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let p = env::temp_dir().join(format!(
            "fschem-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(p.join("schem/nested")).unwrap();
        p
    }
    #[test]
    fn relative_discovery_precedence_and_scoped_environment() {
        let p = fixture();
        fs::write(p.join(FILE_NAME),"schema_version=1\nlibrary_paths=['schem', '$PDK_ROOT/$PDK/libs.tech/xschem']\npdk_root='pdk'\npdk='ihp-sg13g2'\nsimulator='vacask'\n").unwrap();
        let cfg = ProjectConfig::load(&p.join("schem/nested"), None, false).unwrap();
        assert_eq!(
            cfg.library_paths[0],
            fs::canonicalize(p.join("schem")).unwrap()
        );
        assert_eq!(
            cfg.library_paths[1],
            p.join("pdk/ihp-sg13g2/libs.tech/xschem")
        );
        let paths = cfg.libraries(vec![p.join("override")], false).unwrap();
        assert_eq!(paths.roots[0], p.join("override"));
        assert_eq!(paths.variables["PDK"], "ihp-sg13g2");
        assert!(ProjectConfig::load(&p, None, true).unwrap().path.is_none());
        fs::write(p.join("schem/.fschem.toml"), "library_paths=['nested']\n").unwrap();
        assert_eq!(
            ProjectConfig::load(&p.join("schem/nested"), None, false)
                .unwrap()
                .library_paths
                .len(),
            1
        );
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn malformed_configuration_fails_closed() {
        let p = fixture();
        for text in [
            "schema_version=2",
            "simulator='spice'",
            "library_pths=[]",
            "pdk='../../other'",
            "library_paths=['$FSCHEM_UNDEFINED_TEST_VAR']",
        ] {
            fs::write(p.join(FILE_NAME), text).unwrap();
            assert!(ProjectConfig::load(&p, None, false).is_err(), "{text}");
        }
        fs::remove_dir_all(p).unwrap();
    }
}
