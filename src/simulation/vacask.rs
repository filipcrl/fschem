//! Run native VACASK decks without a SPICE converter or Python dependency.
use super::*;
use std::io::Write;

pub(super) fn run(
    document: &Document,
    source: &Path,
    libraries: LibraryPaths,
    raw: &Path,
) -> Result<SimulationFiles, String> {
    run_program(document, source, libraries, raw, OsStr::new("vacask"))
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
    let files = SimulationFiles {
        netlist: raw.with_extension("sim"),
        log: raw.with_extension("log"),
        raw,
        warnings: Vec::new(),
    };
    let deck = netlist::vacask(document, &source, libraries)?;
    netlist::write_spice(&files.netlist, &deck)?;
    let temporary = RunDirectory::new(
        files
            .raw
            .parent()
            .ok_or("raw file needs a parent directory")?,
    )?;
    let input = temporary.0.join("circuit.sim");
    fs::write(&input, deck).map_err(|e| e.to_string())?;
    // Keep simulator output isolated without losing schematic-relative includes
    // and OSDI/Verilog-A loads. Preserve VACASK's built-in search paths.
    let directory = source.parent().unwrap_or(&cwd);
    let config = temporary.0.join("paths.toml");
    let quoted = toml_string(&directory.to_string_lossy())?;
    fs::write(
        &config,
        format!("[Paths]\ninclude_path_prefix = [{quoted}]\nmodule_path_prefix = [{quoted}]\n"),
    )
    .map_err(|e| e.to_string())?;
    let log = fs::File::create(&files.log).map_err(|e| e.to_string())?;
    let mut command = Command::new(program);
    command.args(["-qp", "-sp"]);
    let local_config = directory.join(".vacaskrc.toml");
    if local_config.is_file() {
        command.arg("--extra-tomlfile").arg(local_config);
    }
    let status = command
        .arg("--extra-tomlfile")
        .arg(config)
        .arg(&input)
        .current_dir(&temporary.0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log))
        .status()
        .map_err(|e| {
            run_error(
                &files,
                &format!("Could not start VACASK: {e}. Install vacask and ensure it is on PATH."),
            )
        })?;
    if !status.success() {
        return Err(run_error(&files, &format!("VACASK exited with {status}")));
    }
    // Analysis names belong to the native testbench, not fschem. Collect every
    // top-level .raw file created in this fresh run directory, in filename order.
    let mut plots = fs::read_dir(&temporary.0)
        .map_err(|e| e.to_string())?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    plots.retain(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("raw")));
    plots.sort();
    if plots.is_empty() {
        return Err(run_error(
            &files,
            "VACASK produced no raw data. Add native analysis commands inside a control/endc block and enable result writing.",
        ));
    }
    let combined = temporary.0.join("combined.tmp");
    let mut output = fs::File::create(&combined).map_err(|e| e.to_string())?;
    for path in plots {
        crate::raw::RawFile::open(&path).map_err(|e| {
            run_error(
                &files,
                &format!("Invalid VACASK results {}: {e}", path.display()),
            )
        })?;
        let mut input = fs::File::open(path).map_err(|e| e.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
        output.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    drop(output);
    fs::copy(combined, &files.raw)
        .map_err(|e| format!("Could not save {}: {e}", files.raw.display()))?;
    Ok(files)
}

fn toml_string(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("VACASK search paths cannot contain control characters".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    const DIVIDER: &str = include_str!("../../examples/vacask.sch");

    #[cfg(unix)]
    #[test]
    fn pipeline_combines_plots_and_preserves_results_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = RunDirectory::new(&std::env::temp_dir()).unwrap();
        let output_dir = dir.0.join("results with spaces");
        fs::create_dir(&output_dir).unwrap();
        let program = dir.0.join("fake vacask");
        fs::write(&program, "#!/bin/sh\nset -eu\ntest \"$1\" = -qp\ntest \"$2\" = -sp\ntest \"$3\" = --extra-tomlfile\ngrep -q include_path_prefix \"$4\"\ngrep -q 'analysis op1 op' \"$5\"\nfor n in 0 1; do\nprintf 'Title: divider\\nPlotname: test\\nFlags: real\\nNo. Variables: 1\\nNo. Points: 1\\nVariables:\\n0 vout notype\\nValues:\\n0 2\\n' > \"plot$n.raw\"\ndone\necho simulated\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let document = Document::parse(DIVIDER).unwrap();
        let source = dir.0.join("divider.sch");
        let raw = output_dir.join("divider.raw");
        let files = run_program(
            &document,
            &source,
            LibraryPaths::default(),
            &raw,
            program.as_os_str(),
        )
        .unwrap();
        assert_eq!(crate::raw::RawFile::open(&raw).unwrap().plots.len(), 2);
        assert!(fs::read_to_string(&files.netlist).unwrap().contains("endc"));
        assert!(
            fs::read_to_string(&files.log)
                .unwrap()
                .contains("simulated")
        );
        let previous = fs::read(&raw).unwrap();
        for script in [
            "#!/bin/sh\necho failed >&2\nexit 1\n",
            "#!/bin/sh\nexit 0\n",
            "#!/bin/sh\necho broken > op.raw\n",
        ] {
            fs::write(&program, script).unwrap();
            assert!(
                run_program(
                    &document,
                    &source,
                    LibraryPaths::default(),
                    &raw,
                    program.as_os_str()
                )
                .is_err()
            );
            assert_eq!(fs::read(&raw).unwrap(), previous);
        }
        let error = run_program(
            &document,
            &source,
            LibraryPaths::default(),
            &raw,
            dir.0.join("missing-vacask").as_os_str(),
        )
        .unwrap_err();
        assert!(error.contains("PATH"));
        assert_eq!(fs::read(&raw).unwrap(), previous);
        assert!(
            !fs::read_dir(output_dir)
                .unwrap()
                .any(|e| e.unwrap().file_type().unwrap().is_dir())
        );
    }

    #[test]
    #[ignore = "requires vacask on PATH"]
    fn vacask_runs_native_op_transient_and_ac_and_relative_includes() {
        let dir = RunDirectory::new(&std::env::temp_dir()).unwrap();
        let output_dir = dir.0.join("results with spaces");
        fs::create_dir(&output_dir).unwrap();
        fs::write(dir.0.join("supply.inc"), "parameters supply=3\n").unwrap();
        let document = Document::parse(
            DIVIDER
                .replace("E {}", "E {include \"supply.inc\"}")
                .replace("dc=3", "dc=supply"),
        )
        .unwrap();
        let raw = output_dir.join("divider.raw");
        let files = run(
            &document,
            &dir.0.join("divider.sch"),
            LibraryPaths::default(),
            &raw,
        )
        .unwrap();
        let data = crate::raw::RawFile::open(&files.raw).unwrap();
        assert_eq!(data.plots.len(), 3);
        for plot in &data.plots {
            assert!(plot.points > 0);
            let out = plot.variables.iter().find(|v| v.name == "VOUT").unwrap();
            if plot.name.contains("Operating") || plot.name.contains("Transient") {
                assert!(out.values.iter().all(|v| (v.real - 2.).abs() < 1e-9));
            } else {
                assert!(
                    out.values
                        .iter()
                        .all(|v| (v.real - 2. / 3.).abs() < 1e-9 && v.imaginary.abs() < 1e-9)
                );
            }
        }
        let previous = fs::read(&raw).unwrap();
        let bad = Document::parse(DIVIDER.replace("analysis op1 op", "analysis op1 nonexistent"))
            .unwrap();
        assert!(
            run(
                &bad,
                &dir.0.join("divider.sch"),
                LibraryPaths::default(),
                &raw
            )
            .is_err(),
            "{}",
            fs::read_to_string(&files.log).unwrap()
        );
        assert_eq!(fs::read(raw).unwrap(), previous);
    }
}
