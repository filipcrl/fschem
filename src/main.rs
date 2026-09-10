mod canvas;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit's Objective-C target/action bridge is isolated here.
mod macos_menu;
mod viewer;
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
    let mut roots = Vec::new();
    let mut check = false;
    let mut verbose = false;
    let mut smoke_test = false;
    let mut screenshot = None;
    let mut netlist = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!(
                    "fschem [OPTIONS] [FILE.sch|FILE.sym]\n\n  -L, --library DIR  Add a symbol library search path (repeatable)\n  --netlist OUTPUT  Export FILE.sch as a SPICE deck without opening a window\n  --check PATH...   Check file syntax and lossless retention; recurse through directories\n  --verbose        Show all parser diagnostics in check mode\n  --smoke-test     Open, render three frames, then exit (demo if no file given)\n  --screenshot PNG Capture the egui window after loading\n\nCtrl/Command-O opens; Ctrl/Command-S saves; Ctrl/Command-Z undoes; Ctrl/Command-Shift-Z redoes.\nClick/Shift-click or left-drag selects; G moves; C copies; W wires; Backspace deletes.\nR/X/Y rotate or mirror while moving; left-click places; Esc cancels.\nMiddle/right-drag pans; wheel/pinch zooms; F fits. i: symbol, e: schematic, Shift-E: up.\nFile → Export SPICE netlist exports the current schematic.\nThe stroke font is built in. Source schematic files change only when saved."
                );
                return Ok(());
            }
            Some("--library" | "-L") => roots.push(PathBuf::from(
                args.next().ok_or("--library requires a directory")?,
            )),
            Some("--check") => check = true,
            Some("--netlist") => {
                netlist = Some(PathBuf::from(
                    args.next().ok_or("--netlist requires an output path")?,
                ));
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
    if let Some(output) = netlist {
        if check || smoke_test || screenshot.is_some() || inputs.len() != 1 {
            return Err("--netlist OUTPUT requires one input schematic and cannot be combined with --check, --smoke-test or --screenshot".into());
        }
        let input = std::fs::canonicalize(&inputs[0]).map_err(|e| e.to_string())?;
        if std::fs::canonicalize(&output).ok().as_ref() == Some(&input) {
            return Err("netlist output must differ from the source schematic".into());
        }
        let doc = read_document(&input)?;
        let deck = fschem::netlist::spice(&doc, &input, LibraryPaths::discover(roots))?;
        fschem::netlist::write_spice(&output, &deck)?;
        println!("Exported SPICE netlist to {}", output.display());
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
