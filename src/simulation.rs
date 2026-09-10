//! Generate a deck and run ngspice without depending on the GUI.
use crate::{document::Document, library::LibraryPaths, netlist};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug)]
pub struct SimulationFiles {
    pub netlist: PathBuf,
    pub raw: PathBuf,
    pub log: PathBuf,
    /// Non-blocking ngspice startup diagnostics, retained on successful runs.
    pub warnings: Vec<String>,
}

/// Run the schematic's analyses using `ngspice` on PATH. Relative model/include
/// paths are resolved from the schematic directory, not the output directory.
/// The previous raw file is replaced only after a successful run produces data.
pub fn run(
    document: &Document,
    source: &Path,
    libraries: LibraryPaths,
    raw: &Path,
) -> Result<SimulationFiles, String> {
    run_program(document, source, libraries, raw, OsStr::new("ngspice"))
}

fn run_program(
    document: &Document,
    source: &Path,
    libraries: LibraryPaths,
    raw: &Path,
    program: &OsStr,
) -> Result<SimulationFiles, String> {
    if !raw
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("raw"))
    {
        return Err("Choose a .raw filename for the simulation results".into());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let source = cwd.join(source);
    let raw = cwd.join(raw);
    let mut files = SimulationFiles {
        netlist: raw.with_extension("spice"),
        log: raw.with_extension("log"),
        raw,
        warnings: Vec::new(),
    };
    let deck = netlist::spice(document, &source, libraries)?;
    netlist::write_spice(&files.netlist, &deck)?;
    let temporary = RunDirectory::new(
        files
            .raw
            .parent()
            .ok_or("raw file needs a parent directory")?,
    )?;
    let raw_output = temporary.0.join("simulation.raw");
    let run_netlist = temporary.0.join("simulation.spice");
    fs::write(&run_netlist, save_control_results(&deck)).map_err(|e| e.to_string())?;
    let log = fs::File::create(&files.log).map_err(|e| format!("{}: {e}", files.log.display()))?;
    let stderr = log.try_clone().map_err(|e| e.to_string())?;
    let status = Command::new(program)
        .arg("-b")
        .arg("-r").arg(&raw_output)
        .arg(&run_netlist)
        .current_dir(source.parent().unwrap_or(&cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .status()
        .map_err(|e| format!("Could not start ngspice: {e}. Install ngspice and ensure it is on PATH. Netlist: {}", files.netlist.display()))?;
    if !status.success() {
        return Err(run_error(&files, &format!("ngspice exited with {status}")));
    }
    // Control scripts can finish with exit status zero even when an analysis
    // failed, leaving the previous plot current. Don't publish that as success.
    let log_text = fs::read_to_string(&files.log).unwrap_or_default();
    let diagnostics = LogDiagnostics::parse(&log_text);
    if !diagnostics.errors.is_empty() {
        return Err(run_error(&files, "ngspice reported a simulation error"));
    }
    if !fs::metadata(&raw_output).is_ok_and(|m| m.is_file() && m.len() > 0) {
        return Err(run_error(
            &files,
            "ngspice produced no raw data. Add an analysis such as .op, .tran, .ac or .dc to the schematic",
        ));
    }
    fs::copy(&raw_output, &files.raw)
        .map_err(|e| format!("Could not save {}: {e}", files.raw.display()))?;
    files.warnings = diagnostics.warnings;
    Ok(files)
}

#[derive(Default)]
struct LogDiagnostics {
    errors: Vec<String>,
    warnings: Vec<String>,
}
impl LogDiagnostics {
    fn parse(log: &str) -> Self {
        let mut result = Self::default();
        let mut startup = true;
        for line in log.lines() {
            let text = line.trim();
            let lower = text.to_ascii_lowercase();
            if lower.starts_with("circuit:") || lower.starts_with("doing analysis") {
                startup = false;
            }
            // PDK startup scripts often preload all OSDI models, including
            // optional devices that this circuit does not use. A load failure
            // before the circuit is read is not itself an analysis failure.
            // Missing models actually used by the circuit still produce errors
            // during circuit loading/analysis, which remain fatal below.
            let osdi_load = lower.starts_with("error opening osdi lib ")
                || (lower.starts_with("error: library ")
                    && lower.contains(".osdi")
                    && lower.ends_with("couldn't be loaded!"));
            if startup && osdi_load {
                result.warnings.push(text.to_owned());
            } else if osdi_load
                || lower.starts_with("error:")
                || lower.starts_with("error on line")
                || lower.starts_with("error during")
                || lower.starts_with("fatal")
            {
                result.errors.push(text.to_owned());
            }
        }
        result
    }
}

fn run_error(files: &SimulationFiles, message: &str) -> String {
    let log = fs::read_to_string(&files.log).unwrap_or_default();
    let diagnostics = LogDiagnostics::parse(&log);
    // Include the triggering diagnostics even if a long transient run pushed
    // them outside the log tail. Startup notices also help explain failed runs.
    let diagnostics = diagnostics
        .errors
        .iter()
        .chain(&diagnostics.warnings)
        .take(10)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    // Keep the complete log on disk, but don't flood the error window.
    let excerpt = log
        .lines()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{message}\nLog: {}\n\n{diagnostics}\n\nRecent output:\n{excerpt}",
        files.log.display()
    )
}

/// Batch -r writes dot-card analyses automatically. Analyses executed inside
/// .control need an explicit write. Quit after the final control block to stop
/// batch mode from reopening/truncating the raw file for a second automatic run.
fn save_control_results(deck: &str) -> String {
    let last_end = deck
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            line.split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(".endc"))
        })
        .map(|(i, _)| i)
        .last();
    let mut output = String::new();
    let mut control = false;
    let mut analysis = false;
    for (index, line) in deck.lines().enumerate() {
        let command = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if command == ".control" {
            control = true;
        }
        if control
            && matches!(
                command.as_str(),
                "op" | "tran" | "ac" | "dc" | "noise" | "run" | "sp" | "sens" | "disto"
            )
        {
            analysis = true;
        }
        if control && analysis && matches!(command.as_str(), ".endc" | "quit") {
            output.push_str("write $rawfile\n");
            if command == ".endc" && Some(index) == last_end {
                output.push_str("quit\n");
            }
        }
        if command == ".endc" {
            control = false;
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

struct RunDirectory(PathBuf);
impl RunDirectory {
    fn new(parent: &Path) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = parent.join(format!(
                ".fschem-ngspice-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
}
impl Drop for RunDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIVIDER: &str = include_str!("../examples/spice.sch");

    #[test]
    fn startup_osdi_notices_do_not_mask_analysis_errors() {
        let startup = "Error opening osdi lib \"/pdk/cap_cmomi.osdi\": No such file or directory!\nError: Library /pdk/cap_cmomi.osdi couldn't be loaded!\n";
        let success = format!(
            "{startup}\nCircuit: tg_tb\nNo. of Data Rows : 40086\nPASS: TG forward/reverse conduction and off hold\nbinary raw file simulation.raw\n"
        );
        let diagnostics = LogDiagnostics::parse(&success);
        assert!(diagnostics.errors.is_empty());
        assert_eq!(diagnostics.warnings.len(), 2);
        let failed = LogDiagnostics::parse(&format!("{success}Error: timestep too small\n"));
        assert_eq!(failed.errors, ["Error: timestep too small"]);
        assert_eq!(failed.warnings.len(), 2);
        // Only recognized startup preloads are downgraded, never circuit-time
        // model loading or arbitrary early errors.
        assert_eq!(
            LogDiagnostics::parse(&format!("Circuit: test\n{startup}"))
                .errors
                .len(),
            2
        );
        assert_eq!(
            LogDiagnostics::parse("Error: missing model\nCircuit: test\n")
                .errors
                .len(),
            1
        );
        assert_eq!(
            LogDiagnostics::parse("Fatal error: startup failed\n")
                .errors
                .len(),
            1
        );
    }

    #[test]
    fn errors_before_the_log_tail_are_shown_in_failure_messages() {
        let dir = RunDirectory::new(&std::env::temp_dir()).unwrap();
        let files = SimulationFiles {
            netlist: dir.0.join("test.spice"),
            raw: dir.0.join("test.raw"),
            log: dir.0.join("test.log"),
            warnings: Vec::new(),
        };
        fs::write(
            &files.log,
            format!(
                "Error: missing required model\n{}",
                "ordinary output\n".repeat(80)
            ),
        )
        .unwrap();
        let message = run_error(&files, "ngspice reported a simulation error");
        assert!(message.contains("Error: missing required model"));
        assert!(message.contains("Recent output:"));
    }

    #[test]
    #[ignore = "requires ngspice with OSDI support on PATH"]
    fn missing_optional_startup_osdi_still_publishes_successful_raw_data() {
        let dir = RunDirectory::new(&std::env::temp_dir()).unwrap();
        fs::write(
            dir.0.join(".spiceinit"),
            "osdi fschem_missing_optional_cap.osdi\n",
        )
        .unwrap();
        let source = dir.0.join("test.sch");
        let raw = dir.0.join("test.raw");
        let document = Document::parse(
            DIVIDER.replace("value=\".op\"", "value=\".control\ntran 1n 10n\n.endc\""),
        )
        .unwrap();
        let result = run(&document, &source, LibraryPaths::default(), &raw).unwrap();
        assert!(
            !result.warnings.is_empty(),
            "{}",
            fs::read_to_string(&result.log).unwrap()
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("fschem_missing_optional_cap.osdi"))
        );
        let data = crate::raw::RawFile::open(&result.raw).unwrap();
        assert!(data.plots[0].points > 1);
        let out = data.plots[0]
            .variables
            .iter()
            .find(|v| v.name == "v(vout)")
            .unwrap();
        assert!(out.values.iter().all(|v| (v.real - 2.).abs() < 1e-9));
        let previous = fs::read(&raw).unwrap();
        let failed = Document::parse(
            DIVIDER.replace("value=\".op\"", "value=\".control\nop\ntran bogus\n.endc\""),
        )
        .unwrap();
        assert!(run(&failed, &source, LibraryPaths::default(), &raw).is_err());
        assert_eq!(fs::read(raw).unwrap(), previous);
    }

    #[test]
    fn missing_executable_preserves_previous_results_and_reports_how_to_fix() {
        let dir = RunDirectory::new(&std::env::temp_dir()).unwrap();
        let raw = dir.0.join("result.raw");
        fs::write(&raw, "previous run").unwrap();
        let error = run_program(
            &Document::parse(DIVIDER).unwrap(),
            &dir.0.join("test.sch"),
            LibraryPaths::default(),
            &raw,
            dir.0.join("missing-ngspice").as_os_str(),
        )
        .unwrap_err();
        assert!(error.contains("Could not start ngspice"), "{error}");
        assert!(error.contains("PATH"));
        assert_eq!(fs::read_to_string(raw).unwrap(), "previous run");
        assert!(dir.0.join("result.spice").is_file());
        assert!(
            !fs::read_dir(&dir.0)
                .unwrap()
                .any(|e| e.unwrap().file_type().unwrap().is_dir())
        );
    }

    #[test]
    fn control_results_are_written_before_quit_and_configuration_blocks_are_preserved() {
        let deck =
            "* test\n.control\nset noaskquit\n.endc\n.control\ntran 1n 1u\nquit\n.endc\n.end\n";
        let result = save_control_results(deck);
        assert!(result.contains("set noaskquit\n.endc"));
        assert!(result.contains("tran 1n 1u\nwrite $rawfile\nquit"));
        assert_eq!(
            save_control_results("* test\n.op\n.end\n"),
            "* test\n.op\n.end\n"
        );
    }

    #[test]
    #[ignore = "requires ngspice on PATH"]
    fn ngspice_runs_dot_analyses_and_control_commands_and_keeps_failed_results() {
        let dir = RunDirectory::new(&std::env::temp_dir()).unwrap();
        // Keep the fixture independent of the user's PDK startup configuration.
        fs::write(dir.0.join(".spiceinit"), "").unwrap();
        let output_dir = dir.0.join("results with spaces");
        fs::create_dir(&output_dir).unwrap();
        fs::write(dir.0.join("supply.inc"), ".param supply=3\n").unwrap();
        let source = DIVIDER
            .replace("S {}", "S {.include \"supply.inc\"}")
            .replace("value=3", "value='supply'");
        let raw = output_dir.join("divider.raw");
        for analysis in [
            ".op",
            ".control\nop\n.endc",
            ".control\ntran 1n 10n\nquit\n.endc",
        ] {
            let document =
                Document::parse(source.replace("value=\".op\"", &format!("value=\"{analysis}\"")))
                    .unwrap();
            let files = run(
                &document,
                &dir.0.join("divider.sch"),
                LibraryPaths::default(),
                &raw,
            )
            .unwrap();
            let bytes = fs::read(&files.raw).unwrap();
            let header = String::from_utf8_lossy(&bytes);
            assert!(header.contains("v(vout)"), "{header}");
            assert!(header.contains("No. Points:"));
            assert!(files.netlist.is_file());
            assert!(files.log.is_file());
        }
        let previous = fs::read(&raw).unwrap();
        let bad = Document::parse(source.replace("supply.inc", "missing.inc")).unwrap();
        let error = run(
            &bad,
            &dir.0.join("divider.sch"),
            LibraryPaths::default(),
            &raw,
        )
        .unwrap_err();
        assert!(error.contains("ngspice exited"), "{error}");
        assert!(error.contains("missing.inc"), "{error}");
        assert_eq!(fs::read(&raw).unwrap(), previous);
        let failed_control = Document::parse(
            source.replace("value=\".op\"", "value=\".control\nop\ntran bogus\n.endc\""),
        )
        .unwrap();
        assert!(
            run(
                &failed_control,
                &dir.0.join("divider.sch"),
                LibraryPaths::default(),
                &raw
            )
            .is_err()
        );
        assert_eq!(fs::read(&raw).unwrap(), previous);
        let no_analysis =
            Document::parse(source.replace("value=\".op\"", "value=\"* no analysis\"")).unwrap();
        assert!(
            run(
                &no_analysis,
                &dir.0.join("divider.sch"),
                LibraryPaths::default(),
                &raw
            )
            .is_err()
        );
        assert_eq!(fs::read(&raw).unwrap(), previous);
    }
}
