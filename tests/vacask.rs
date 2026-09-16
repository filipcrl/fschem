use fschem::{document::Document, library::LibraryPaths, netlist};
use std::path::Path;

const EXAMPLE: &str = include_str!("../examples/vacask.sch");
fn deck(source: &str) -> Result<String, String> {
    netlist::vacask(
        &Document::parse(source).unwrap(),
        Path::new("test.sch"),
        LibraryPaths::default(),
    )
}

#[test]
fn native_testbench_and_shared_connectivity() {
    let result = deck(EXAMPLE).unwrap();
    assert!(result.contains("R1 (VIN VOUT) fschem_resistor r=10k"));
    assert!(result.contains("R2 (VOUT 0) fschem_resistor r=20k"));
    assert!(result.contains("V1 (VIN 0) fschem_vsource dc=3 mag=1"));
    assert!(result.contains("control\n  abort always\n  analysis op1 op"));
    assert!(result.contains("mode=\"dec\""));
    assert_eq!(result.matches("load \"resistor.osdi\"").count(), 1);
    assert!(!result.contains(".end"));
    assert_eq!(result, deck(EXAMPLE).unwrap());
}

#[test]
fn native_format_and_model_are_expanded_without_spice_fallback() {
    let source = EXAMPLE.replace("name=R1 value=10k", "name=R1 value=10k vacask_format=\"@name (@@N @@P) custom r=@value\" vacask_model=\"model custom resistor\"");
    let result = deck(&source).unwrap();
    assert!(result.contains("R1 (VOUT VIN) custom r=10k"));
    assert!(result.contains("model custom resistor"));
    let unknown = EXAMPLE.replace("type=resistor", "type=custom");
    assert!(deck(&unknown).unwrap_err().contains("vacask_format"));
}

#[test]
fn native_and_spice_testbenches_can_coexist() {
    let source = include_str!("../examples/spice.sch")
        .replace(
            "name=SIM value=\".op\"",
            "name=SIM value=\".op\" vacask_code=\"control\n  analysis native op\nendc\"",
        )
        .replace("E {}", "E {parameters native_parameter=3}");
    let native = deck(&source).unwrap();
    assert!(native.contains("analysis native op"));
    assert!(native.contains("parameters native_parameter=3"));
    assert!(!native.contains(".op"));
    let spice = netlist::spice(
        &Document::parse(source.as_str()).unwrap(),
        Path::new("test.sch"),
        LibraryPaths::default(),
    )
    .unwrap();
    assert!(spice.contains(".op"));
    assert!(!spice.contains("analysis native"));
    assert!(!spice.contains("native_parameter"));
}

#[test]
fn rejects_spice_commands_and_source_waveforms() {
    assert!(
        deck(include_str!("../examples/spice.sch"))
            .unwrap_err()
            .contains("SPICE directives")
    );
    let source = EXAMPLE.replace(
        "value=3 vacask_params=\"dc=3 mag=1\"",
        "value=\"PULSE(0 1 0 1n 1n 5n 10n)\"",
    );
    assert!(deck(&source).unwrap_err().contains("vacask_params"));
    let ignored = EXAMPLE.replace("name=R2 value=20k", "name=R2 value=20k vacask_ignore=true");
    assert!(!deck(&ignored).unwrap().contains("R2 ("));
}
