"""Explicit external GUI launching; importing this module never launches a GUI."""
from pathlib import Path
import subprocess

from .schematic import PathInput


def open_run(path: PathInput, *, executable: PathInput = "fschem") -> subprocess.Popen:
    """Launch a separate GUI with ``--open-run PATH``; return its process handle.

    ``executable`` is one installed GUI program pathname/name, never a shell
    command. The Python wheel does not bundle the GUI. The GUI validates the
    manifest and opens artifacts read-only; no simulator or Python script runs.
    """
    path = Path(path).expanduser().resolve(strict=True)
    manifest = path / "run.json" if path.is_dir() else path
    if not manifest.is_file():
        raise ValueError(f"not a run manifest: {manifest}")
    return subprocess.Popen([str(executable), "--open-run", str(manifest)])
