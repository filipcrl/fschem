//! Private, headless bridge. Public API and NumPy ownership live in Python.
use fschem::{library::LibraryPaths, netlist, raw::RawFile, scene::LoadedDocument};
use pyo3::{exceptions::PyValueError, prelude::*};
use std::path::PathBuf;

type Diagnostic = (String, usize, String);
type RawVariable = (String, String, Vec<f64>, Vec<f64>);
type RawPlot = (String, String, bool, Option<usize>, Vec<RawVariable>);

#[pyclass(module = "fschem._core")]
struct Schematic {
    loaded: LoadedDocument,
    paths: LibraryPaths,
}

#[pymethods]
impl Schematic {
    #[new]
    fn new(
        py: Python<'_>,
        path: PathBuf,
        library_paths: Vec<PathBuf>,
        discover: bool,
    ) -> PyResult<Self> {
        py.detach(move || {
            let paths = if discover {
                LibraryPaths::discover(library_paths)
            } else {
                LibraryPaths {
                    roots: library_paths,
                }
            };
            let loaded =
                LoadedDocument::open(&path, paths.clone()).map_err(PyValueError::new_err)?;
            Ok(Self { loaded, paths })
        })
    }

    fn source(&self) -> &str {
        self.loaded.document.original()
    }

    fn diagnostics(&self) -> Vec<Diagnostic> {
        self.loaded
            .scene
            .diagnostics
            .iter()
            .map(|d| {
                (
                    d.file.to_string_lossy().into_owned(),
                    d.line,
                    d.message.clone(),
                )
            })
            .collect()
    }

    fn portable(&self, py: Python<'_>, backend: &str,
        parameters: std::collections::BTreeMap<String, String>,
        local_parameters: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
        exclude: std::collections::BTreeSet<String>,
    ) -> PyResult<(String, bool, std::collections::BTreeMap<String, String>)> {
        py.detach(|| {
            let circuit = netlist::portable::compile(&self.loaded.document,
                self.loaded.path.as_ref().expect("opened source"), self.paths.clone(), backend,
                netlist::portable::Options { parameters, local_parameters, exclude })
                .map_err(PyValueError::new_err)?;
            Ok((circuit.deck, circuit.ihp, circuit.signals))
        })
    }

    fn netlist(&self, py: Python<'_>, backend: &str) -> PyResult<String> {
        py.detach(|| {
            let source = self
                .loaded
                .path
                .as_ref()
                .expect("opened document has a path");
            match backend {
                "ngspice" => netlist::spice(&self.loaded.document, source, self.paths.clone()),
                "vacask" => netlist::vacask(&self.loaded.document, source, self.paths.clone()),
                _ => Err(format!("unsupported native backend: {backend}")),
            }
            .map_err(PyValueError::new_err)
        })
    }
}

#[pyfunction]
fn read_raw(py: Python<'_>, path: PathBuf) -> PyResult<Vec<RawPlot>> {
    py.detach(move || {
        let raw = RawFile::open(&path).map_err(PyValueError::new_err)?;
        Ok(raw
            .plots
            .into_iter()
            .map(|plot| {
                let scale = plot.scale();
                (
                    plot.title,
                    plot.name,
                    plot.complex,
                    scale,
                    plot.variables
                        .into_iter()
                        .map(|v| {
                            let (real, imaginary) =
                                v.values.into_iter().map(|s| (s.real, s.imaginary)).unzip();
                            (v.name, v.kind, real, imaginary)
                        })
                        .collect(),
                )
            })
            .collect())
    })
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Schematic>()?;
    m.add_function(wrap_pyfunction!(read_raw, m)?)?;
    Ok(())
}
