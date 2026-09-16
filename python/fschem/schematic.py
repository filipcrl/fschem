"""Ordinary Python wrappers over the private Rust document implementation."""
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
                 discover_libraries: bool = True, technology=None):
        self.technology = technology
        self.root = Path(root).resolve()
        self.library_paths = tuple(self._resolve(p) for p in library_paths)
        self.discover_libraries = discover_libraries

    def _resolve(self, path: PathInput) -> Path:
        return (self.root / path).resolve()

    def load(self, path: PathInput) -> "Schematic":
        source = self._resolve(path)
        native = _core.Schematic(source, list(self.library_paths), self.discover_libraries)
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

    def netlist(self, *, backend: Backend = "ngspice") -> str:
        """Generate a legacy/native deck without writing files or running a simulator."""
        return self._native.netlist(backend)
