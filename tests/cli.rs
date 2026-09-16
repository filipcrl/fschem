#![cfg(feature = "gui")]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fschem-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("spice.sch"),
            include_str!("../examples/spice.sch"),
        )
        .unwrap();
        fs::write(
            path.join("vacask.sch"),
            include_str!("../examples/vacask.sch"),
        )
        .unwrap();
        Self(path)
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fschem"));
        cmd.current_dir(&self.0).stdin(Stdio::null());
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn project_configuration_loads_libraries_and_scoped_pdk_without_exports() {
    let dir = Workspace::new();
    fs::create_dir_all(dir.0.join("lib/mock")).unwrap();
    fs::create_dir_all(dir.0.join("override")).unwrap();
    fs::create_dir_all(dir.0.join("schem")).unwrap();
    let symbol = "v {xschem file_version=1.3}\nK {type=resistor format=\"@name @pinlist @value\" template=\"name=R value=3k\"}\nB 5 -2 -2 2 2 {name=P dir=inout pinnumber=1}\nB 5 98 -2 102 2 {name=N dir=inout pinnumber=2}\n";
    fs::write(dir.0.join("lib/mock/configured.sym"),symbol).unwrap();
    fs::write(dir.0.join("override/configured.sym"),symbol.replace("3k","9k")).unwrap();
    fs::write(dir.0.join(".fschem.toml"),"pdk_root='lib'\npdk='mock'\nlibrary_paths=['$PDK_ROOT/$PDK']\nsimulator='vacask'\n").unwrap();
    fs::write(dir.0.join("schem/config.sch"),"v {xschem file_version=1.3}\nC {$env(PDK_ROOT)/$env(PDK)/configured.sym} 0 0 0 0 {name=R1}\nC {configured.sym} 0 100 0 0 {name=R2}\n").unwrap();
    let output=dir.command().env_remove("PDK_ROOT").env_remove("PDK").args(["-L","override","--netlist","configured.spice","schem/config.sch"]).output().unwrap();
    success(&output);
    let deck=fs::read_to_string(dir.0.join("configured.spice")).unwrap();
    assert!(deck.lines().any(|s|s.starts_with("R1 ")&&s.ends_with("3k")),"{deck}");
    assert!(deck.lines().any(|s|s.starts_with("R2 ")&&s.ends_with("9k")),"{deck}");
    assert!(!dir.command().env_remove("PDK_ROOT").env_remove("PDK").args(["--no-config","--netlist","missing.spice","schem/config.sch"]).output().unwrap().status.success());
    fs::write(dir.0.join(".fschem.toml"),"unknown_key=true").unwrap();
    assert!(!dir.run(&["--check","schem/config.sch"]).status.success());
    success(&dir.run(&["--no-config","--check","schem/config.sch"]));
    assert!(!dir.run(&["--config",".fschem.toml","--no-config","--check","schem/config.sch"]).status.success());
}

#[test]
fn exports_both_formats_without_a_window() {
    let dir = Workspace::new();
    success(&dir.run(&["--netlist", "default.spice", "spice.sch"]));
    success(&dir.run(&[
        "spice.sch",
        "--netlist-format",
        "spice",
        "--netlist",
        "explicit.spice",
    ]));
    assert_eq!(
        fs::read(dir.0.join("default.spice")).unwrap(),
        fs::read(dir.0.join("explicit.spice")).unwrap()
    );
    let spice = fs::read_to_string(dir.0.join("default.spice")).unwrap();
    assert!(spice.contains("R1 VIN VOUT 10k"));
    assert!(spice.ends_with(".end\n"));
    success(&dir.run(&[
        "--netlist-format",
        "VACASK",
        "--netlist",
        "native deck.sim",
        "vacask.sch",
    ]));
    let native = fs::read_to_string(dir.0.join("native deck.sim")).unwrap();
    assert!(native.contains("R1 (VIN VOUT) fschem_resistor r=10k"));
    assert!(native.contains("analysis op1 op"));
    assert!(!native.contains(".end"));
}

#[test]
fn invalid_options_and_output_collisions_fail_before_writing() {
    let dir = Workspace::new();
    for args in [
        vec!["--netlist-format"],
        vec!["--simulator"],
        vec!["--simulate"],
        vec![
            "--netlist-format",
            "bogus",
            "--netlist",
            "out.sim",
            "spice.sch",
        ],
        vec!["--simulator", "bogus", "--simulate", "out.raw", "spice.sch"],
        vec!["--netlist-format", "vacask", "spice.sch"],
        vec!["--simulator", "vacask", "spice.sch"],
        vec!["--netlist", "out.sim", "--simulate", "out.raw", "spice.sch"],
        vec!["--simulate", "out.raw", "--check", "spice.sch"],
        vec!["--simulate", "out.raw", "--smoke-test", "spice.sch"],
        vec![
            "--simulate",
            "out.raw",
            "--screenshot",
            "out.png",
            "spice.sch",
        ],
        vec!["--simulate", "out.raw"],
        vec!["--simulate", "out.raw", "spice.sch", "vacask.sch"],
        vec!["--netlist", "spice.sch", "spice.sch"],
        vec!["--simulate", "spice.sch", "spice.sch"],
        vec!["--simulate", "out.txt", "spice.sch"],
    ] {
        let output = dir.run(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    assert_eq!(
        fs::read_to_string(dir.0.join("spice.sch")).unwrap(),
        include_str!("../examples/spice.sch")
    );
    assert!(!dir.0.join("out.raw").exists());
    assert!(!dir.0.join("out.sim").exists());
}

#[cfg(unix)]
#[test]
fn simulator_selection_runs_the_matching_backend_and_reports_failures() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = Workspace::new();
    let bin = dir.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let raw = "Title: test\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n0 VOUT notype\nValues:\n0 2\n";
    for (program, script) in [
        (
            "ngspice",
            format!(
                "#!/bin/sh\nset -eu\ntest \"$1\" = -b\ntest \"$2\" = -r\nprintf '{raw}' > \"$3\"\necho ngspice-ran\n"
            ),
        ),
        (
            "vacask",
            format!(
                "#!/bin/sh\nset -eu\ntest \"$1\" = -qp\ntest \"$2\" = -sp\nprintf '{raw}' > op.raw\necho vacask-ran\n"
            ),
        ),
    ] {
        let path = bin.join(program);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (simulator, schematic, extension) in [
        ("ngspice", "spice.sch", "spice"),
        ("vacask", "vacask.sch", "sim"),
    ] {
        let result = dir
            .command()
            .env("PATH", &bin)
            .args([
                "--simulate",
                "result.raw",
                "--simulator",
                simulator,
                schematic,
            ])
            .output()
            .unwrap();
        success(&result);
        assert!(String::from_utf8_lossy(&result.stdout).contains("simulation complete"));
        assert!(dir.0.join(format!("result.{extension}")).is_file());
        assert!(
            fs::read_to_string(dir.0.join("result.log"))
                .unwrap()
                .contains(&format!("{simulator}-ran"))
        );
        assert_eq!(
            fschem::raw::RawFile::open(&dir.0.join("result.raw"))
                .unwrap()
                .plots
                .len(),
            1
        );
    }
    // No selector retains the historical ngspice default.
    success(
        &dir.command()
            .env("PATH", &bin)
            .args(["--simulate", "default.raw", "spice.sch"])
            .output()
            .unwrap(),
    );
    assert!(dir.0.join("default.spice").exists());
    fs::remove_file(bin.join("vacask")).unwrap();
    let before = fs::read(dir.0.join("result.raw")).unwrap();
    let result = dir
        .command()
        .env("PATH", &bin)
        .args([
            "--simulate",
            "result.raw",
            "--simulator",
            "vacask",
            "vacask.sch",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Could not start VACASK"));
    assert_eq!(fs::read(dir.0.join("result.raw")).unwrap(), before);
    // Protect the input even when a generated companion file is a symlink to it.
    symlink(dir.0.join("vacask.sch"), dir.0.join("danger.sim")).unwrap();
    let result = dir.run(&[
        "--simulate",
        "danger.raw",
        "--simulator",
        "vacask",
        "vacask.sch",
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("must differ"));
    assert_eq!(
        fs::read_to_string(dir.0.join("vacask.sch")).unwrap(),
        include_str!("../examples/vacask.sch")
    );
}

#[test]
#[ignore = "requires ngspice and vacask on PATH"]
fn real_simulators_run_from_the_cli() {
    let dir = Workspace::new();
    // Keep this divider independent of the user's PDK startup configuration.
    fs::write(dir.0.join(".spiceinit"), "").unwrap();
    for (simulator, input) in [("ngspice", "spice.sch"), ("vacask", "vacask.sch")] {
        let raw = format!("{simulator}.raw");
        success(&dir.run(&["--simulator", simulator, "--simulate", &raw, input]));
        let data = fschem::raw::RawFile::open(&dir.0.join(raw)).unwrap();
        assert!(!data.plots.is_empty());
    }
}

#[test]
fn ir_is_json_only_and_rejects_conflicts_and_source_overwrite() {
    let dir = Workspace::new();
    for stage in ["parsed", "resolved"] {
        let output = dir.run(&["--dump-ir", stage, "spice.sch"]);
        success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["stage"], stage);
    }
    let source = fs::read(dir.0.join("spice.sch")).unwrap();
    for args in [
        vec!["--dump-ir", "parsed", "--output", "spice.sch", "spice.sch"],
        vec!["--dump-ir", "parsed", "--check", "spice.sch"],
        vec!["--dump-ir", "bad", "spice.sch"],
        vec!["--ir-config", "missing.json", "spice.sch"],
    ] { assert!(!dir.run(&args).status.success()); }
    assert_eq!(source, fs::read(dir.0.join("spice.sch")).unwrap());
    success(&dir.run(&["--dump-ir", "parsed", "--output", "ir.json", "spice.sch"]));
    serde_json::from_slice::<serde_json::Value>(&fs::read(dir.0.join("ir.json")).unwrap()).unwrap();
}

#[test]
fn lowered_ir_config_and_backend_selection() {
    let dir = Workspace::new();
    fs::write(dir.0.join("portable.sch"), include_str!("../examples/portable.sch")).unwrap();
    fs::write(dir.0.join("config.json"), include_str!("../examples/portable-ir.json")).unwrap();
    let output = dir.run(&["--dump-ir", "lowered", "--simulator", "vacask", "--ir-config", "config.json", "portable.sch"]);
    success(&output);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["backend"], "vacask");
    for config in ["{broken", r#"{"unknown":1}"#] {
        fs::write(dir.0.join("config.json"), config).unwrap();
        let output = dir.run(&["--dump-ir", "lowered", "--ir-config", "config.json", "portable.sch"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    fs::write(dir.0.join("existing.json"), "keep").unwrap();
    assert!(!dir.run(&["--dump-ir", "parsed", "--output", "existing.json", "portable.sch"]).status.success());
    assert_eq!(fs::read_to_string(dir.0.join("existing.json")).unwrap(), "keep");
}

#[test]
fn invalid_run_manifests_fail_before_gui_startup() {
    let dir = Workspace::new();
    fs::write(dir.0.join("run.json"), "{broken").unwrap();
    for args in [
        vec!["--open-run", "run.json"],
        vec!["--open-run", "."],
        vec!["--open-run", "run.json", "--check", "spice.sch"],
        vec!["--open-run", "run.json", "--dump-ir", "parsed", "spice.sch"],
    ] {
        let output = dir.run(&args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}
