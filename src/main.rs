mod canvas;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit's Objective-C target/action bridge is isolated here.
mod macos_menu;
mod viewer;

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
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!(
                    "fschem [OPTIONS] [FILE.sch|FILE.sym]\n\n  -L, --library DIR  Add a symbol library search path (repeatable)\n  --check PATH...   Check file syntax and lossless retention; recurse through directories\n  --verbose        Show all parser diagnostics in check mode\n  --smoke-test     Open, render three frames, then exit (demo if no file given)\n  --screenshot PNG Capture the egui window after loading\n\nCtrl/Command-O opens a file. Left-click selects; middle/right-drag pans.\nWheel/pinch zooms; F fits. i: symbol, e: schematic, Shift-E: up one level.\nThe stroke font is built in. No schematic files are modified by this read-only milestone."
                );
                return Ok(());
            }
            Some("--library" | "-L") => roots.push(PathBuf::from(
                args.next().ok_or("--library requires a directory")?,
            )),
            Some("--check") => check = true,
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
