from pathlib import Path
import os
import subprocess
import sys

import pytest
import fschem

ROOT = Path(__file__).resolve().parents[2]


def test_native_export_snapshot_and_headless_check(tmp_path):
    source = tmp_path / "divider.sch"
    source.write_bytes((ROOT / "examples/spice.sch").read_bytes())
    project = fschem.Project(tmp_path, discover_libraries=False)
    circuit = project.load("divider.sch")
    assert circuit.path == source.resolve()
    assert circuit.source == source.read_text()
    source.write_text("changed after loading")
    assert "R1 VIN VOUT 10k" in circuit.netlist()
    assert "R2 VOUT 0 20k" in circuit.netlist()
    assert circuit.netlist().endswith(".op\n.end\n")
    assert circuit.check(backend="ngspice") == ()
    with pytest.raises(ValueError, match="SPICE directives"):
        circuit.netlist(backend="vacask")
    with pytest.raises(ValueError, match="unsupported native backend"):
        circuit.netlist(backend="unknown")


def test_native_vacask():
    circuit = fschem.Project(ROOT, discover_libraries=False).load("examples/vacask.sch")
    deck = circuit.netlist(backend="vacask")
    assert "R1 (VIN VOUT) fschem_resistor r=10k" in deck
    assert "analysis op1 op" in deck
    assert circuit.check(backend="vacask") == ()


def test_diagnostics_and_errors(tmp_path):
    source = tmp_path / "bad.sch"
    source.write_text("C {missing.sym} 0 0 0 0 {name=R1}\n")
    circuit = fschem.Project(tmp_path, discover_libraries=False).load(source)
    assert circuit.diagnostics
    diagnostic = circuit.diagnostics[0]
    assert diagnostic.file == source
    assert diagnostic.line == 1
    assert "missing.sym" in diagnostic.message
    with pytest.raises(ValueError):
        circuit.check(backend="ngspice")
    with pytest.raises(ValueError, match="absent.sch"):
        circuit.project.load("absent.sch")
    source.write_text("C {unterminated")
    with pytest.raises(ValueError, match="line"):
        circuit.project.load(source)


def test_project_paths_do_not_mutate_process(tmp_path):
    cwd, env = Path.cwd(), dict(os.environ)
    lib = tmp_path / "symbols"
    lib.mkdir()
    (lib / "r.sym").write_text('K {type=resistor format="@name @pinlist @value"}\n'
                               'B 5 -1 -1 1 1 {name=P}\nB 5 9 -1 11 1 {name=N}\n')
    (tmp_path / "c.sch").write_text('C {r.sym} 0 0 0 0 {name=R1 value=1k}\n')
    project = fschem.Project(tmp_path, library_paths=["symbols"], discover_libraries=False)
    assert project.library_paths == (lib,)
    assert "R1 net1 net2 1k" in project.load("c.sch").netlist()
    assert Path.cwd() == cwd
    assert dict(os.environ) == env


def test_installed_import_outside_checkout(tmp_path):
    result = subprocess.run([sys.executable, "-I", "-c",
                             "import fschem; from fschem import _core; "
                             "print(_core.__file__)"], cwd=tmp_path,
                            capture_output=True, text=True, check=True)
    assert "site-packages" in result.stdout
