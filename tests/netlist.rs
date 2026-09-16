use fschem::{document::Document, library::LibraryPaths, netlist};
use std::path::Path;

const RESISTOR: &str = r#"[
K {type=resistor format="@name @pinlist @value" template="name=R1 value=1k"}
B 5 -1 -11 1 -9 {name=P pinnumber=2}
B 5 -1 9 1 11 {name=N pinnumber=1}
]
"#;
const LABEL: &str = "[\nK {type=label template=\"name=l1 lab=0\"}\nB 5 -1 -1 1 1 {name=p}\n]\n";

fn deck(source: &str) -> Result<String, String> {
    netlist::spice(
        &Document::parse(source).unwrap(),
        Path::new("test.sch"),
        LibraryPaths::default(),
    )
}

#[test]
fn demo_reuses_embedded_symbols_and_has_deterministic_floating_nets() {
    let source = include_str!("../examples/demo.sch");
    let result = deck(source).unwrap();
    assert!(
        result.contains("R1 VIN VOUT 10k\nR2 VOUT GND 20k\nR3 net1 net2 4.7k\n"),
        "{result}"
    );
    assert_eq!(result, deck(source).unwrap());
    assert!(result.ends_with(".end\n"));
}

#[test]
fn connects_t_junctions_diagonals_labels_and_transformed_pins_in_record_order() {
    let result = deck(&format!(
        r#"N -30 0 30 0 {{lab=INPUT}}
N 0 0 0 10 {{}}
N 0 30 30 60 {{}}
N 30 60 50 60 {{}}
C {{r.sym}} 0 20 0 0 {{name=R1 value=2k}}
{RESISTOR}C {{label.sym}} 50 60 0 0 {{name=g1}}
{LABEL}C {{r.sym}} 100 0 1 1 {{name=R2}}
C {{label.sym}} 90 0 0 0 {{name=l2 lab=INPUT}}
C {{label.sym}} 110 0 0 0 {{name=l3 lab=OUTPUT}}
"#
    ))
    .unwrap();
    assert!(result.contains("R1 INPUT 0 2k"), "{result}");
    // Rotation is applied after mirroring; pinnumber does not reorder pins.
    assert!(result.contains("R2 OUTPUT INPUT 1k"), "{result}");
}

#[test]
fn wire_crossings_do_not_short_and_disconnected_same_labels_connect() {
    let result = deck(&format!(
        r#"N -20 0 20 0 {{lab=A}}
N 0 -20 0 20 {{lab=B}}
C {{r.sym}} 20 10 0 0 {{name=R1}}
{RESISTOR}C {{r.sym}} 0 30 0 0 {{name=R2}}
C {{label.sym}} 20 20 0 0 {{name=l1 lab=C}}
{LABEL}C {{label.sym}} 0 40 0 0 {{name=l2 lab=C}}
"#
    ))
    .unwrap();
    assert!(result.contains("R1 A C 1k\nR2 B C 1k"), "{result}");
}

#[test]
fn format_pin_tokens_commands_models_defaults_and_ignore() {
    let result = deck(r#"S {.param gain=2}
C {device.sym} 0 0 0 0 {name=M1 value=7}
[
K {type=mos format="@spiceprefix@name @@D @#1:net_name @model @extra value=@value" template="name=M1 model=nfet value=1" device_model=".model nfet nmos"}
B 5 -1 -11 1 -9 {name=D}
B 5 -1 9 1 11 {name=S}
]
C {device.sym} 50 0 0 0 {name=M2}
C {missing.sym} 0 0 0 0 {spice_ignore=true}
C {source.sym} 100 0 0 0 {name=V1 savecurrent=true}
[
K {type=vsource format="@name @pinlist @value@savecurrent" template="name=V1 value=3 savecurrent=false"}
B 5 -1 -11 1 -9 {name=P}
B 5 -1 9 1 11 {name=M}
]
C {code.sym} 0 0 0 0 {name=SIM value=".tran 1n 1u
.end"}
[
K {type=netlist_commands format=@value}
]
"#).unwrap();
    assert!(
        result.contains(".param gain=2\nM1 net1 net2 nfet  value=7"),
        "{result}"
    );
    assert!(result.contains("M2 net3 net4 nfet  value=1"));
    assert!(result.contains("V1 net5 net6 3\n.save I(V1)\n.tran 1n 1u"));
    assert_eq!(result.matches(".model nfet nmos").count(), 1);
    assert_eq!(result.lines().filter(|line| *line == ".end").count(), 1);
    assert!(result.ends_with(".model nfet nmos\n.end\n"));
}

#[test]
fn invalid_circuits_are_errors_instead_of_partial_decks() {
    for (source, message) in [
        ("C {missing.sym} 0 0 0 0 {}".to_owned(), "missing symbol"),
        (
            "N 0 0 10 0 {lab=A}\nN 10 0 20 0 {lab=B}".to_owned(),
            "conflicting labels",
        ),
        ("N 0 0 10 0 {lab=D[7:0]}".to_owned(), "bus/vector"),
        (
            format!("C {{r.sym}} 0 0 0 0 {{name=R1}}\n{RESISTOR}C {{r.sym}} 50 0 0 0 {{name=r1}}"),
            "duplicate instance",
        ),
        (
            format!("C {{r.sym}} 0 0 0 0 {{name=R1 value=tcleval(foo)}}\n{RESISTOR}"),
            "Tcl evaluation",
        ),
        (
            format!("C {{r.sym}} 0 0 0 0 {{name=R1 format=@unknown}}\n{RESISTOR}"),
            "unresolved format",
        ),
    ] {
        let error = deck(&source).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
    assert!(netlist::write_spice(Path::new("never-write.sch"), "test").is_err());
}

#[test]
fn hierarchy_uses_symbol_pin_order_and_emits_one_definition() {
    let dir = std::env::temp_dir().join(format!("fschem-netlist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = format!(
        r#"C {{r.sym}} 0 0 0 0 {{name=R1 value=5k}}
{RESISTOR}C {{port.sym}} 0 10 0 0 {{name=p1 lab=OUT}}
[
K {{type=opin}}
B 5 -1 -1 1 1 {{name=p}}
]
C {{input.sym}} 0 -10 0 0 {{name=p2 lab=IN}}
[
K {{type=ipin}}
B 5 -1 -1 1 1 {{name=p}}
]
C {{code.sym}} 0 0 0 0 {{name=s1 only_toplevel=true value=".tran 1n 1u"}}
[
K {{type=netlist_commands format=@value}}
]
"#
    );
    std::fs::write(dir.join("cell.sch"), &child).unwrap();
    let source = r#"C {cell.sym} 0 0 0 0 {name=X1}
[
K {type=subcircuit format="@name @pinlist @symname" template="name=X1"}
B 5 -1 -11 1 -9 {name=IN}
B 5 -1 9 1 11 {name=OUT}
]
C {cell.sym} 100 0 0 0 {name=X2}
"#;
    let doc = Document::parse(source).unwrap();
    let result = netlist::spice(&doc, &dir.join("top.sch"), LibraryPaths::default()).unwrap();
    assert!(
        result.contains("X1 net1 net2 cell\nX2 net3 net4 cell"),
        "{result}"
    );
    assert_eq!(result.matches(".subckt cell IN OUT").count(), 1);
    assert!(
        result.contains("R1 IN OUT 5k\n.ends cell\n.end"),
        "{result}"
    );
    assert!(!result.contains(".tran"));
    let native_doc = Document::parse(source.replace(
        "type=subcircuit",
        "type=subcircuit vacask_format=\"@name (@pinlist) @symname\"",
    )).unwrap();
    let native = netlist::vacask(&native_doc, &dir.join("top.sch"), LibraryPaths::default()).unwrap();
    assert!(native.contains("X1 (net1 net2) cell\nX2 (net3 net4) cell"), "{native}");
    assert_eq!(native.matches("subckt cell (IN OUT)").count(), 1);
    assert!(native.contains("R1 (IN OUT) fschem_resistor r=5k\nends"), "{native}");
    assert!(!native.contains(".tran"));
    std::fs::write(dir.join("cell.sch"), format!("{source}\n{child}")).unwrap();
    let error = netlist::spice(&doc, &dir.join("top.sch"), LibraryPaths::default()).unwrap_err();
    assert!(error.contains("recursive subcircuit"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires XSCHEM_TEST_LIBRARY pointing to an external Xschem library"]
fn installed_xschem_symbols_generate_a_divider() {
    let root = std::env::var_os("XSCHEM_TEST_LIBRARY").expect("set XSCHEM_TEST_LIBRARY");
    let doc = Document::parse(
        r#"N 0 -30 100 -30 {lab=VIN}
N 0 30 0 90 {}
N 0 90 100 90 {}
C {devices/vsource.sym} 0 0 0 0 {name=V1 value=3}
C {devices/res.sym} 100 0 0 0 {name=R1 value=10k}
C {devices/res.sym} 100 60 0 0 {name=R2 value=20k}
C {devices/lab_pin.sym} 100 30 0 0 {name=l1 lab=VOUT}
C {devices/gnd.sym} 0 90 0 0 {name=l2 lab=0}
C {devices/code_shown.sym} 200 0 0 0 {name=SIM value=.op}
"#,
    )
    .unwrap();
    let result = netlist::spice(
        &doc,
        Path::new("divider.sch"),
        LibraryPaths {
            roots: vec![root.into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.contains("V1 VIN 0 3\n"), "{result}");
    assert!(
        result.contains("R1 VIN VOUT 10k m=1\nR2 VOUT 0 20k m=1\n"),
        "{result}"
    );
    assert!(result.contains(".op"));
}
