//! Private, headless bridge. Public API and NumPy ownership live in Python.
use fschem::{library::LibraryPaths, netlist, raw::RawFile, scene::LoadedDocument};
use pyo3::{exceptions::PyValueError, prelude::*};
use std::path::PathBuf;

type Diagnostic = (String, usize, String);
type RawVariable = (String, String, String, String, Vec<f64>, Vec<f64>);
type ExpressionVariable = (String, String, Vec<f64>, Vec<f64>);
type Calculated = (String, String, Vec<f64>, Vec<f64>);
type RawPlot = (String, String, bool, Option<usize>, Vec<RawVariable>);

#[pyclass(module = "fschem._core")]
struct Schematic {
    loaded: LoadedDocument,
    paths: LibraryPaths,
}

#[pymethods]
impl Schematic {
    #[new]
    #[pyo3(signature = (path, library_paths, discover, variables=None))]
    fn new(
        py: Python<'_>,
        path: PathBuf,
        library_paths: Vec<PathBuf>,
        discover: bool,
        variables: Option<std::collections::HashMap<String,String>>,
    ) -> PyResult<Self> {
        py.detach(move || {
            let mut paths = if discover {
                LibraryPaths::discover(library_paths)
            } else {
                LibraryPaths {
                    roots: library_paths,
                    ..Default::default()
                }
            };
            paths.variables = variables.unwrap_or_default();
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

    fn portable(
        &self,
        py: Python<'_>,
        backend: &str,
        parameters: std::collections::BTreeMap<String, String>,
        local_parameters: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, String>,
        >,
        exclude: std::collections::BTreeSet<String>,
        sources: std::collections::BTreeMap<String, (String, Vec<String>, String)>,
    ) -> PyResult<(
        String,
        bool,
        std::collections::BTreeMap<String, String>,
        String,
    )> {
        py.detach(|| {
            let circuit = netlist::portable::compile(
                &self.loaded.document,
                self.loaded.path.as_ref().expect("opened source"),
                self.paths.clone(),
                backend,
                netlist::portable::Options {
                    parameters,
                    local_parameters,
                    exclude,
                    sources,
                },
            )
            .map_err(PyValueError::new_err)?;
            let snapshot = netlist::lowered_snapshot(
                &circuit,
                self.loaded.path.as_ref().expect("opened source"),
                backend,
            )
            .to_string();
            Ok((circuit.deck, circuit.ihp, circuit.signals, snapshot))
        })
    }

    fn dump_ir(
        &self,
        py: Python<'_>,
        stage: &str,
        backend: &str,
        config: &str,
    ) -> PyResult<String> {
        py.detach(|| {
            netlist::dump_ir(
                &self.loaded.document,
                self.loaded.path.as_ref().expect("opened source"),
                self.paths.clone(),
                stage,
                backend,
                config,
            )
            .map_err(PyValueError::new_err)
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
                            (v.name, v.kind, v.native_kind, v.unit, real, imaginary)
                        })
                        .collect(),
                )
            })
            .collect())
    })
}

#[pyfunction]
fn calculate(py: Python<'_>, expression: String, variables: Vec<ExpressionVariable>, scale: Option<usize>) -> PyResult<Calculated> {
    py.detach(move || {
        let points = variables.first().map(|v| v.2.len()).unwrap_or(0);
        if points == 0 || variables.iter().any(|v| v.2.len() != points || v.3.len() != points) || scale.is_some_and(|i| i >= variables.len()) {
            return Err(PyValueError::new_err("invalid expression dataset dimensions/scale"));
        }
        // The evaluator's RawPlot scale convention is the first variable. Reorder
        // vectors here rather than silently differentiating against another signal.
        let mut variables = variables;
        if let Some(index) = scale { variables.swap(0, index); }
        let plot = fschem::raw::RawPlot {
            title: "Python expression".into(),
            name: if scale.is_some() { "Sweep" } else { "Operating Point" }.into(),
            complex: true, points,
            variables: variables.into_iter().map(|(name,kind,real,imag)| fschem::raw::Variable {
                name, unit: fschem::units::Unit::parse(&kind).label, native_kind: kind.clone(), kind,
                values: real.into_iter().zip(imag).map(|(real,imaginary)| fschem::raw::Sample { real,imaginary }).collect(),
            }).collect(),
        };
        let result = fschem::units::calculate(&plot, &expression).map_err(PyValueError::new_err)?;
        let (real, imaginary) = result.values.into_iter().map(|s| (s.real,s.imaginary)).unzip();
        Ok((result.kind,result.unit,real,imaginary))
    })
}

#[pyfunction]
fn unit(kind: &str) -> String { fschem::units::Unit::parse(kind).label }

#[pyfunction]
fn project_config(start: PathBuf, explicit: Option<PathBuf>, disabled: bool) -> PyResult<String> {
    let config = fschem::config::ProjectConfig::load(&start, explicit.as_deref(), disabled).map_err(PyValueError::new_err)?;
    serde_json::to_string(&config).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Schematic>()?;
    m.add_function(wrap_pyfunction!(read_raw, m)?)?;
    m.add_function(wrap_pyfunction!(calculate, m)?)?;
    m.add_function(wrap_pyfunction!(unit, m)?)?;
    m.add_function(wrap_pyfunction!(project_config, m)?)?;
    Ok(())
}
