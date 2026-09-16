mod canvas;
mod graph;
mod run_artifact;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit's Objective-C target/action bridge is isolated here.
mod macos_menu;
mod schematic_graphs;
mod viewer;
mod text_viewer;
mod waveform_lod;
mod waveforms;

use fschem::{
    document::DiagnosticKind,
    library::{LibraryPaths, read_document},
};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fschem: {error}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), String> {
    let mut inputs = Vec::new();
    let mut dump_ir = None;
    let mut ir_config = None;
    let mut ir_output = None;
    let mut roots = Vec::new();
    let mut check = false;
    let mut open_as_text = false;
    let mut open_run = None;
    let mut verbose = false;
    let mut smoke_test = false;
    let mut screenshot = None;
    let mut netlist = None;
    let mut netlist_format = None;
    let mut simulate = None;
    let mut simulator = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!(
                    "fschem [OPTIONS] [FILE.sch|FILE.sym|TEXT]\n\n  -L, --library DIR  Add a symbol library search path (repeatable)\n  --netlist OUTPUT  Export a netlist without opening a window\n  --netlist-format FORMAT\n                   Netlist format: spice (default) or vacask\n  --simulate RAW   Run FILE.sch headlessly; save .raw, netlist and .log files\n  --simulator NAME Backend for --simulate/--dump-ir: ngspice (default) or vacask\n  --dump-ir STAGE  Inspect parsed/resolved/lowered IR as JSON (no GUI)\n  --ir-config FILE Portable lowering JSON config\n  --output FILE    Write IR JSON to a fresh file instead of stdout\n  --check PATH...   Check file syntax and lossless retention; recurse through directories\n  --verbose        Show all parser diagnostics in check mode\n  --open-run PATH  Inspect a Python run directory/manifest; never execute\n  --open-text      Open any file as read-only text (including .sch)\n  --smoke-test     Open, render three frames, then exit (demo if no file given)\n  --screenshot PNG Capture the egui window after loading\n\nCtrl/Command-O opens; Ctrl/Command-S saves; Ctrl/Command-Z undoes; Ctrl/Command-Shift-Z redoes.\nClick/Shift-click or left-drag selects; G moves; C copies; W wires; Backspace deletes.\nR/X/Y rotate or mirror while moving; left-click places; Esc cancels.\nMiddle/right-drag pans; wheel/pinch zooms; F fits. i: symbol, e: schematic, Shift-E: up.\nFile → Export SPICE netlist exports the current schematic.\nThe stroke font is built in. Source schematic files change only when saved."
                );
                return Ok(());
            }
            Some("--library" | "-L") => roots.push(PathBuf::from(
                args.next().ok_or("--library requires a directory")?,
            )),
            Some("--dump-ir") => dump_ir = Some(args.next().ok_or("--dump-ir requires a stage")?.to_string_lossy().into_owned()),
            Some("--ir-config") => ir_config = Some(PathBuf::from(args.next().ok_or("--ir-config requires a file")?)),
            Some("--output") => ir_output = Some(PathBuf::from(args.next().ok_or("--output requires a file")?)),
            Some("--check") => check = true,
            Some("--open-run") => open_run = Some(PathBuf::from(args.next().ok_or("--open-run requires a directory or manifest")?)),
            Some("--open-text") => open_as_text = true,
            Some("--netlist") => {
                netlist = Some(PathBuf::from(
                    args.next().ok_or("--netlist requires an output path")?,
                ));
            }
            Some("--netlist-format") => {
                let value = args
                    .next()
                    .ok_or("--netlist-format requires spice or vacask")?;
                netlist_format = Some(
                    match value.to_str().map(str::to_ascii_lowercase).as_deref() {
                        Some("spice" | "ngspice") => fschem::simulation::Simulator::Ngspice,
                        Some("vacask") => fschem::simulation::Simulator::Vacask,
                        _ => {
                            return Err(format!(
                                "unknown netlist format {value:?}; choose spice or vacask"
                            ));
                        }
                    },
                );
            }
            Some("--simulate") => {
                simulate = Some(PathBuf::from(
                    args.next()
                        .ok_or("--simulate requires an output .raw path")?,
                ));
            }
            Some("--simulator") => {
                let value = args
                    .next()
                    .ok_or("--simulator requires ngspice or vacask")?;
                simulator = Some(
                    match value.to_str().map(str::to_ascii_lowercase).as_deref() {
                        Some("ngspice") => fschem::simulation::Simulator::Ngspice,
                        Some("vacask") => fschem::simulation::Simulator::Vacask,
                        _ => {
                            return Err(format!(
                                "unknown simulator {value:?}; choose ngspice or vacask"
                            ));
                        }
                    },
                );
            }
            Some("--verbose") => verbose = true,
            Some("--smoke-test") => smoke_test = true,
            Some("--screenshot") => {
                screenshot = Some(PathBuf::from(
                    args.next()
                        .ok_or("--screenshot requires an output PNG path")?,
                ))
            }
            Some("--") => {
                inputs.extend(args.map(PathBuf::from));
                break;
            }
            Some(s) if s.starts_with('-') => {
                return Err(format!("unknown option {s:?}; use --help"));
            }
            _ => inputs.push(PathBuf::from(arg)),
        }
    }
    if let Some(stage) = dump_ir {
        if check || open_as_text || open_run.is_some() || netlist.is_some() || simulate.is_some() || smoke_test || screenshot.is_some() || inputs.len() != 1 || netlist_format.is_some() {
            return Err("--dump-ir requires one schematic and cannot be combined with other actions".into());
        }
        let input = std::fs::canonicalize(&inputs[0]).map_err(|e| e.to_string())?;
        let config = match &ir_config {
            Some(path) => std::fs::read_to_string(path).map_err(|e| e.to_string())?,
            None => "{}".into(),
        };
        let backend = match simulator.unwrap_or_default() {
            fschem::simulation::Simulator::Ngspice => "ngspice",
            fschem::simulation::Simulator::Vacask => "vacask",
        };
        let json = fschem::netlist::dump_ir(&*read_document(&input)?, &input, LibraryPaths::discover(roots), &stage, backend, &config)?;
        if let Some(output) = ir_output {
            protect_source(&input, &output)?;
            if let Some(config) = ir_config { protect_source(&std::fs::canonicalize(config).map_err(|e| e.to_string())?, &output)?; }
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&output)
                .map_err(|e| format!("{} (IR output must be a fresh file): {e}", output.display()))?;
            writeln!(file, "{json}").map_err(|e| e.to_string())?;
        } else { println!("{json}"); }
        return Ok(());
    }
    if ir_config.is_some() || ir_output.is_some() { return Err("--ir-config and --output require --dump-ir".into()); }
    let is_open_run = open_run.is_some();
    if let Some(path) = open_run {
        if check || open_as_text || netlist.is_some() || simulate.is_some() || !inputs.is_empty() { return Err("--open-run cannot be combined with other actions or inputs".into()); }
        inputs.push(run_artifact::RunArtifact::open(&path)?.path);
    }
    if open_as_text && (check || netlist.is_some() || simulate.is_some() || inputs.len() != 1) {
        return Err("--open-text requires one file and cannot be combined with headless modes".into());
    }
    if netlist_format.is_some() && netlist.is_none() {
        return Err("--netlist-format requires --netlist OUTPUT; simulation uses its simulator's native format".into());
    }
    if simulator.is_some() && simulate.is_none() {
        return Err("--simulator requires --simulate OUTPUT.raw".into());
    }
    if netlist.is_some() && simulate.is_some() {
        return Err("--netlist and --simulate are separate modes; --simulate already saves its generated netlist".into());
    }
    if netlist.is_some() || simulate.is_some() {
        if check || smoke_test || screenshot.is_some() || inputs.len() != 1 {
            return Err("--netlist and --simulate require one input schematic and cannot be combined with --check, --smoke-test or --screenshot".into());
        }
        let input = std::fs::canonicalize(&inputs[0])
            .map_err(|e| format!("{}: {e}", inputs[0].display()))?;
        let doc = read_document(&input)?;
        let libraries = LibraryPaths::discover(roots);
        if let Some(output) = netlist {
            protect_source(&input, &output)?;
            let format = netlist_format.unwrap_or_default();
            let deck = match format {
                fschem::simulation::Simulator::Ngspice => {
                    fschem::netlist::spice(&doc, &input, libraries)?
                }
                fschem::simulation::Simulator::Vacask => {
                    fschem::netlist::vacask(&doc, &input, libraries)?
                }
            };
            fschem::netlist::write_spice(&output, &deck)?;
            println!(
                "Exported {} netlist to {}",
                match format {
                    fschem::simulation::Simulator::Ngspice => "SPICE",
                    fschem::simulation::Simulator::Vacask => "VACASK",
                },
                output.display()
            );
        } else if let Some(output) = simulate {
            let simulator = simulator.unwrap_or_default();
            for path in [
                output.clone(),
                output.with_extension("log"),
                output.with_extension(match simulator {
                    fschem::simulation::Simulator::Ngspice => "spice",
                    fschem::simulation::Simulator::Vacask => "sim",
                }),
            ] {
                protect_source(&input, &path)?;
            }
            let files = fschem::simulation::run_with_simulator(
                simulator, &doc, &input, libraries, &output,
            )?;
            println!(
                "{} simulation complete\nRaw: {}\nNetlist: {}\nLog: {}",
                simulator.name(),
                files.raw.display(),
                files.netlist.display(),
                files.log.display()
            );
            for warning in files.warnings {
                eprintln!("Warning: {warning}");
            }
        }
        return Ok(());
    }
    if check {
        if inputs.is_empty() {
            return Err("--check needs one or more files/directories".into());
        }
        return check_files(inputs, verbose);
    }
    if inputs.len() > 1 {
        return Err("open one file at a time, or use --check for a corpus".into());
    }
    let options = viewer::Options {
        input: inputs.pop(),
        open_as_text,
        open_run: is_open_run,
        libraries: LibraryPaths::discover(roots),
        smoke_test,
        screenshot,
    };
    let success = Arc::new(AtomicBool::new(false));
    let app_success = Arc::clone(&success);
    eframe::run_native(
        "fschem",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1280., 850.])
                .with_min_inner_size([720., 480.])
                .with_app_id("dev.fschem.fschem"),
            ..Default::default()
        },
        Box::new(move |cc| {
            let viewer = viewer::Viewer::new(&cc.egui_ctx, options, app_success);
            #[cfg(target_os = "macos")]
            let viewer = {
                let mut viewer = viewer;
                viewer.install_native_menu(&cc.egui_ctx);
                viewer
            };
            Ok(Box::new(viewer))
        }),
    )
    .map_err(|e| e.to_string())?;
    if smoke_test && !success.load(Ordering::Relaxed) {
        return Err("viewer smoke test did not finish successfully".into());
    }
    Ok(())
}

fn protect_source(input: &Path, output: &Path) -> Result<(), String> {
    if std::fs::canonicalize(output).ok().as_deref() == Some(input) {
        return Err(format!(
            "output {} must differ from the source schematic",
            output.display()
        ));
    }
    Ok(())
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                collect(&entry.path(), files)?;
            } else if entry.path().extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("sch") || ext.eq_ignore_ascii_case("sym")
            }) {
                files.push(entry.path());
            }
        }
    } else {
        files.push(path.to_path_buf());
    }
    Ok(())
}
fn check_files(inputs: Vec<PathBuf>, verbose: bool) -> Result<(), String> {
    let mut files = Vec::new();
    for input in inputs {
        collect(&input, &mut files)?;
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err("no .sch or .sym files found".into());
    }
    let (mut records, mut diagnostics, mut errors) = (0, 0, 0);
    for path in &files {
        match read_document(path) {
            Ok(document) => {
                records += document.records.len();
                diagnostics += document.diagnostics.len();
                // Unknown future records are supported losslessly. Malformed
                // known records are errors for corpus verification, not data loss.
                let malformed = document
                    .diagnostics
                    .iter()
                    .any(|d| d.kind != DiagnosticKind::UnsupportedRecord);
                if malformed {
                    errors += 1;
                }
                if verbose || malformed {
                    for d in &document.diagnostics {
                        eprintln!("{}:{}: {}", path.display(), d.line, d.message);
                    }
                }
                let original = std::fs::read(path).map_err(|e| e.to_string())?;
                if document.original().as_bytes() != original {
                    return Err(format!("lossless retention failed: {}", path.display()));
                }
            }
            Err(error) => {
                errors += 1;
                eprintln!("{error}");
            }
        }
    }
    println!(
        "Checked {} files, {records} top-level records, {diagnostics} diagnostics, {errors} failed files. Original bytes retained exactly for successfully parsed documents.",
        files.len()
    );
    if errors > 0 {
        Err(format!("{errors} file(s) failed compatibility checks"))
    } else {
        Ok(())
    }
}
