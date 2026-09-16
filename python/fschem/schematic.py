"""Ordinary Python wrappers over the private Rust document implementation."""
import json
from dataclasses import dataclass
from pathlib import Path
from os import PathLike
from typing import Iterable, Literal

from . import _core

PathInput = str | PathLike[str]
Backend = Literal["ngspice", "vacask"]


@dataclass(frozen=True)
class Diagnostic:
    file: Path
    line: int
    message: str


class Project:
    """Resolve schematic and library paths relative to a fixed root.

    Set discover_libraries=False to disable legacy environment/system discovery.
    No working-directory or environment changes are made.
    """

    def __init__(self, root: PathInput = ".", *,
                 library_paths: Iterable[PathInput] = (),
                 discover_libraries: bool = True, technology=None,
                 config: PathInput | Literal[False] | None = None):
        self.root = Path(root).expanduser().resolve()
        if config is True:
            raise ValueError("config must be a path, None for discovery, or False to disable")
        explicit = None if config is None or config is False else Path(config).expanduser()
        self.config = json.loads(_core.project_config(self.root, explicit, config is False))
        self.config_path = Path(self.config["path"]) if self.config["path"] else None
        self.library_paths = tuple(self._resolve(p) for p in library_paths) + tuple(Path(p) for p in self.config["library_paths"])
        self.discover_libraries = discover_libraries
        self.technology = technology
        if technology is None and self.config["pdk_root"] and self.config["pdk"] == "ihp-sg13g2":
            from .technology import IHP
            self.technology = IHP(self.config["pdk_root"])
        self._variables = {}
        if self.config["pdk_root"]:
            self._variables["PDK_ROOT"] = self.config["pdk_root"]
        if self.config["pdk"]:
            self._variables["PDK"] = self.config["pdk"]

    def _resolve(self, path: PathInput) -> Path:
        return (self.root / Path(path).expanduser()).resolve()

    def load(self, path: PathInput) -> "Schematic":
        source = self._resolve(path)
        native = _core.Schematic(source, list(self.library_paths), self.discover_libraries, self._variables)
        return Schematic(self, source, native)


class Schematic:
    """Loaded source snapshot; referenced library files are resolved by Rust.

    Use Project.load to construct. Native netlisting preserves backend-specific
    code; it does not translate analyses or apply experiment parameter overrides.
    """

    def __init__(self, project: Project, path: Path, native: _core.Schematic):
        self.project = project
        self.path = path
        self._native = native

    @property
    def source(self) -> str:
        return self._native.source()

    @property
    def diagnostics(self) -> tuple[Diagnostic, ...]:
        return tuple(Diagnostic(Path(f), line, message)
                     for f, line, message in self._native.diagnostics())

    def check(self, *, backend: Backend | None = None) -> tuple[Diagnostic, ...]:
        """Return load diagnostics; optionally validate native export (raises ValueError).

        This is not electrical rule checking. Diagnostics have no severity because
        the underlying scene loader does not classify them.
        """
        if backend is not None:
            self.netlist(backend=backend)
        return self.diagnostics

    def dump_ir(self, stage: str, *, backend: Backend = "ngspice", config=None) -> dict:
        """Return a detached, versioned parsed/resolved/lowered snapshot.

        Uses the same Rust stages as netlisting; never executes a simulator.
        Portable config is accepted only for lowered snapshots.
        """
        import json
        if config is not None and not isinstance(config, dict):
            raise ValueError("IR config must be a mapping")
        return json.loads(self._native.dump_ir(stage, backend, json.dumps(config or {}, allow_nan=False)))

    def netlist(self, *, backend: Backend = "ngspice") -> str:
        """Generate a legacy/native deck without writing files or running a simulator."""
        return self._native.netlist(backend)
