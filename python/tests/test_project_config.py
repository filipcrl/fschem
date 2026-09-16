from pathlib import Path
import pytest
from fschem import Project


def test_nearest_project_config_and_cli_equivalent(tmp_path, monkeypatch):
    root = tmp_path / "project"
    child = root / "schem"
    child.mkdir(parents=True)
    pdk = tmp_path / "pdk"
    symbol_dir = pdk / "ihp-sg13g2/libs.tech/xschem"
    symbol_dir.mkdir(parents=True)
    (root / ".fschem.toml").write_text(
        'schema_version = 1\npdk_root = "../pdk"\npdk = "ihp-sg13g2"\n'
        'library_paths = ["$PDK_ROOT/$PDK/libs.tech/xschem", "schem"]\n'
        'simulator = "vacask"\n')
    monkeypatch.delenv("PDK_ROOT", raising=False)
    monkeypatch.delenv("PDK", raising=False)
    project = Project(child, library_paths=["override"], discover_libraries=False)
    assert project.config_path == root / ".fschem.toml"
    assert project.library_paths == (child / "override", symbol_dir, child)
    assert project.technology.root == pdk
    import os
    assert "PDK_ROOT" not in os.environ
    (symbol_dir / "r.sym").write_text('v {xschem version=3.4.8 file_version=1.2}\nK {type=resistor template="name=R value=1k"}\nL 4 0 0 10 0 {}\n')
    (child / "example.sch").write_text('v {xschem version=3.4.8 file_version=1.2}\nC {$env(PDK_ROOT)/$env(PDK)/libs.tech/xschem/r.sym} 0 0 0 0 {name=R1}\n')
    assert not project.load("example.sch").diagnostics
    disabled = Project(child, config=False, discover_libraries=False)
    assert disabled.config_path is None
    assert not disabled.library_paths
    assert disabled.technology is None


def test_invalid_configuration_does_not_silently_fall_back(tmp_path):
    path = tmp_path / ".fschem.toml"
    path.write_text('library_pths = []\n')
    with pytest.raises(ValueError, match="unknown field"):
        Project(tmp_path)
    assert Project(tmp_path, config=False).config_path is None
    custom = tmp_path / "alternate.toml"
    custom.write_text('library_paths = ["lib"]\n')
    assert Project(tmp_path, config=custom).library_paths == (tmp_path / "lib",)
