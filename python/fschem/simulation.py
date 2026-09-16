"""Explicit native experiments and durable, versioned run artifacts."""
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import threading
import time
from types import MappingProxyType
from typing import Mapping

from ._native import ngspice_inputs, vacask_config
from .result import read_raw
from .schematic import Schematic, PathInput, Backend

SCHEMA_VERSION = 1
_TERMINAL = {"succeeded", "failed", "cancelled", "timed_out"}
_ENV_VALUES = {"PATH", "LANG", "LC_ALL", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS",
               "SPICE_LIB_DIR", "SPICE_EXEC_DIR", "SPICE_SCRIPTS", "NGSPICE_INPUT_DIR"}


class UnsupportedPlatform(RuntimeError):
    pass


class SimulationError(RuntimeError):
    """Execution failed; ``run`` retains the manifest, decks and logs."""
    def __init__(self, message, run):
        super().__init__(message)
        self.run = run


def _now():
    return datetime.now(timezone.utc).isoformat()


def _digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _platform():
    if os.name != "posix":
        raise UnsupportedPlatform("native simulation execution currently requires POSIX process groups")


def _atomic(directory, data):
    temporary = directory / ".run.json.tmp"
    with temporary.open("w") as stream:
        json.dump(data, stream, indent=2, ensure_ascii=False, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, directory / "run.json")


def _artifact(directory, name):
    if not isinstance(name, str) or Path(name).is_absolute():
        raise ValueError("artifact path must be relative")
    path = (directory / name).resolve()
    if not path.is_relative_to(directory) or path == directory:
        raise ValueError("artifact escapes run directory")
    return path


def _stop(process):
    # Kill the group even if the leader has already exited: children may remain.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    time.sleep(0.05)
    process.poll()  # Reap the leader before probing its remaining process group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


class Simulation:
    """Explicit native or portable experiment; portable methods return self."""
    def __init__(self, circuit: Schematic, *, mode: str, parameters=None,
                 corners=None, temperature_c=None, local_parameters=None, exclude=()):
        from ._portable import expressions, number
        if mode not in {"native", "portable"}:
            raise ValueError("mode must be 'native' or 'portable'")
        if mode == "native" and (parameters is not None or corners is not None or
                                 temperature_c is not None or local_parameters is not None or exclude):
            raise ValueError("native mode does not accept portable parameter/corner/temperature overrides")
        self.circuit = circuit
        self.mode = mode
        self.parameters = MappingProxyType(expressions(parameters))
        self.local_parameters = MappingProxyType({k: MappingProxyType(expressions(v))
                                                  for k, v in (local_parameters or {}).items()})
        self.exclude = frozenset(exclude)
        self.corners = corners
        self.temperature_c = number(27 if temperature_c is None else temperature_c, "temperature_c")
        if self.temperature_c <= -273.15:
            raise ValueError("temperature must exceed absolute zero")
        self._analyses = []

    def with_parameters(self, parameters=None, **overrides):
        """Return a new experiment; never mutate this experiment or source files."""
        import copy
        if self.mode != "portable":
            raise ValueError("with_parameters requires portable mode")
        from ._portable import expressions
        updated = dict(self.parameters)
        updated.update(expressions(parameters))
        updated.update(expressions(overrides))
        result = copy.copy(self)
        result.parameters = MappingProxyType(updated)
        result._analyses = copy.deepcopy(self._analyses)
        return result

    def _analysis(self, kind, name, save, **kwargs):
        from ._portable import name as validate_name
        if self.mode != "portable":
            raise ValueError("Python analyses require portable mode")
        validate_name(name)
        if any(a["name"].lower() == name.lower() for a in self._analyses):
            raise ValueError("duplicate analysis name")
        if isinstance(save, str):
            raise ValueError("save must be a sequence of canonical node paths")
        self._analyses.append(dict(kind=kind, name=name, save=tuple(save), **kwargs))
        return self

    def op(self, *, name="op", save=()):
        return self._analysis("op", name, save)

    def tran(self, *, stop, output_step, max_step=None, name="tran", save=()):
        from ._portable import number
        stop = number(stop, "stop", positive=True)
        output_step = number(output_step, "output_step", positive=True)
        max_step = number(output_step if max_step is None else max_step, "max_step", positive=True)
        return self._analysis("tran", name, save, stop=stop, output_step=output_step, max_step=max_step)

    def prepare(self, *, backend: Backend):
        if self.mode == "portable":
            from ._portable import prepare
            return prepare(self, backend)
        original = self.circuit.netlist(backend=backend)
        files = {}
        minimum = 1
        if backend == "ngspice":
            deck, files, count = ngspice_inputs(original, self.circuit.path)
            minimum = max(1, count)
            filename = "circuit.spice"
        elif backend == "vacask":
            deck = original
            files["vacask.toml"] = vacask_config(self.circuit.path)
            filename = "circuit.sim"
            minimum = max(1, len(re.findall(r"(?im)^\s*analysis\s+", deck)))
        else:
            raise ValueError(f"unsupported native backend: {backend}")
        files.update({filename: deck, "original.deck": original,
                      "source.sch": self.circuit.source})
        return PreparedRun(backend, self.circuit.path, filename,
                           MappingProxyType(files), minimum)

    def run(self, *, backend: Backend, directory: PathInput, **kwargs):
        return self.prepare(backend=backend).run(directory=directory, **kwargs)


@dataclass(frozen=True)
class PreparedRun:
    backend: str
    source: Path
    deck_path: str
    files: Mapping[str, str]
    minimum_plots: int
    mode: str = "native"
    portable_metadata: Mapping | None = None

    @property
    def deck(self):
        return self.files[self.deck_path]

    def write(self, directory: PathInput):
        """Write a fresh prepared run without launching anything."""
        directory = Path(directory).resolve()
        directory.mkdir(parents=True, exist_ok=True)
        # Atomic ownership claim also serializes competing writers to empty dirs.
        claim = directory / ".fschem-claim"
        fd = os.open(claim, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(fd)
        try:
            if any(p != claim for p in directory.iterdir()):
                raise FileExistsError(f"run directory is not empty: {directory}")
            for name, text in self.files.items():
                path = _artifact(directory, name)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            data = {
                "schema_version": SCHEMA_VERSION, "status": "prepared",
                "backend": self.backend, "mode": self.mode, "source": str(self.source),
                "created_at": _now(), "started_at": None, "finished_at": None,
                "parameters": None, "analyses": {"origin": "native_deck", "minimum_plots": self.minimum_plots},
                "options": {}, "execution": None,
                "artifacts": {"deck": self.deck_path, "original_deck": "original.deck",
                              "source_snapshot": "source.sch", "log": "simulator.log",
                              "inputs": list(self.files)},
                "input_sha256": {name: _digest(directory / name) for name in self.files},
                "results": [], "diagnostics": [],
            }
            if self.portable_metadata is not None:
                data["portable"] = self.portable_metadata
                data["parameters"] = dict(self.portable_metadata["parameters"])
                data["analyses"].update(origin="portable", specifications=self.portable_metadata["analyses"])
            _atomic(directory, data)
        finally:
            claim.unlink()
        return Run.open(directory)

    def run(self, *, directory: PathInput, timeout=None, executable=None, env=None,
            cancel: threading.Event | None = None):
        _platform()
        return self.write(directory).execute(timeout=timeout, executable=executable,
                                              env=env, cancel=cancel)


class Run:
    """Reopenable artifact handle. Failed execution raises SimulationError(run=self)."""
    def __init__(self, directory, manifest):
        self.directory = directory
        self._manifest = manifest

    @classmethod
    def open(cls, directory: PathInput):
        directory = Path(directory).resolve()
        data = json.loads((directory / "run.json").read_text())
        if data.get("schema_version") != SCHEMA_VERSION:
            raise ValueError("unsupported run manifest schema_version")
        if data.get("status") not in _TERMINAL | {"prepared", "running"}:
            raise ValueError("invalid run status")
        if data.get("backend") not in {"ngspice", "vacask"} or data.get("mode") not in {"native", "portable"}:
            raise ValueError("invalid run backend/mode")
        for key in ("deck", "original_deck", "source_snapshot", "log"):
            _artifact(directory, data["artifacts"][key])
        for name in data["artifacts"]["inputs"]:
            _artifact(directory, name)
        for item in data["results"]:
            _artifact(directory, item["path"])
        run = cls(directory, data)
        if run.status == "succeeded":
            run._read_results()  # Never trust a published success without its raw data.
        return run

    @property
    def manifest(self):
        """A detached JSON-compatible snapshot; mutating it cannot alter the run."""
        return json.loads(json.dumps(self._manifest))

    @property
    def status(self):
        return self._manifest["status"]

    def _read_results(self):
        results = {}
        cached = {}
        for item in self._manifest["results"]:
            path = _artifact(self.directory, item["path"])
            if item["path"] not in cached:
                if _digest(path) != item["sha256"]:
                    raise ValueError(f"raw artifact changed: {item['path']}")
                cached[item["path"]] = read_raw(path)
            result = cached[item["path"]][item["plot_index"]]
            if result.name != item["name"] or list(result.signals) != item["signals"]:
                raise ValueError("raw metadata differs from manifest")
            results[item["id"]] = result
        if not results:
            raise ValueError("successful run has no results")
        return results

    @property
    def results(self):
        if self.status != "succeeded":
            raise ValueError(f"results unavailable for {self.status} run")
        return MappingProxyType(self._read_results())

    def result(self, name):
        results = self.results
        if name in results:
            return results[name]
        matches = [results[item["id"]] for item in self._manifest["results"]
                   if item["name"] == name or item["analysis_name"] == name]
        if len(matches) != 1:
            raise KeyError(f"unknown or ambiguous analysis {name!r}; use a result ID: {list(results)}")
        return matches[0]

    def execute(self, *, timeout=None, executable=None, env=None,
                cancel: threading.Event | None = None):
        _platform()
        if timeout is not None and (not isinstance(timeout, (float, int)) or not 0 < timeout < float("inf")):
            raise ValueError("timeout must be finite and positive")
        if self.status != "prepared":
            raise ValueError("only a prepared run may execute; use a fresh directory")
        lock = self.directory / ".execution-lock"
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(fd)
        # Do not let a stale handle overwrite a completed manifest on failure.
        try:
            if json.loads((self.directory / "run.json").read_text())["status"] != "prepared":
                raise ValueError("run has already executed")
        except BaseException:
            lock.unlink()
            raise
        process = None
        data = self._manifest
        try:
            for name, digest in data["input_sha256"].items():
                if _digest(_artifact(self.directory, name)) != digest:
                    raise ValueError(f"prepared input changed: {name}")
            if list(self.directory.glob("*.raw")) or (self.directory / "simulator.log").exists():
                raise ValueError("prepared run already contains simulator outputs")
            environment = dict(os.environ)
            if env is not None:
                environment.update(env)
            program = os.fspath(executable or data["backend"])
            resolved = shutil.which(program, path=environment.get("PATH"))
            if resolved is not None:
                program = str(Path(resolved).resolve())
            command = ([program, "-b", "-r", "simulation.raw", data["artifacts"]["deck"]]
                       if data["backend"] == "ngspice" else
                       [program, "-qp", "-sp", "--extra-tomlfile", "vacask.toml", data["artifacts"]["deck"]])
            data["status"] = "running"
            data["started_at"] = _now()
            data["options"] = {"timeout_seconds": timeout}
            data["execution"] = {"argv": command, "cwd": str(self.directory),
                                 "executable": program, "version": None,
                                 "executable_sha256": _digest(Path(program)) if resolved else None,
                                 "environment": {k: environment[k] for k in sorted(_ENV_VALUES & environment.keys())},
                                 "environment_override_keys": sorted(env or {}),
                                 "environment_policy": "inherited plus explicit overrides; values allowlisted",
                                 "returncode": None}
            _atomic(self.directory, data)
            with (self.directory / "simulator.log").open("wb") as log:
                process = subprocess.Popen(command, cwd=self.directory, env=environment,
                                           stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                           start_new_session=True)
                start = time.monotonic()
                while True:
                    if cancel is not None and cancel.is_set():
                        data["status"] = "cancelled"
                        raise RuntimeError("simulation cancelled")
                    if timeout is not None and time.monotonic() - start >= timeout:
                        data["status"] = "timed_out"
                        raise RuntimeError(f"simulation timed out after {timeout} seconds")
                    if process.poll() is not None:
                        break
                    time.sleep(0.02)
                _stop(process)
                data["execution"]["returncode"] = process.returncode
                if process.returncode:
                    raise RuntimeError(f"simulator exited with status {process.returncode}")
            text = (self.directory / "simulator.log").read_text(errors="replace")
            # Native control scripts can exit zero after a failed analysis.
            if re.search(r"(?im)^\s*(?:error\b|fatal\b|panic\b)|analysis.*(?:failed|aborted)|simulation interrupted", text):
                raise RuntimeError("simulator log reports an analysis error; see simulator.log")
            # Banner is provenance without a second executable invocation.
            banner = re.search(r"(?im)^.*(?:ngspice[- ]\d|vacask \d)[^\n]*", text)
            data["execution"]["version"] = banner.group().strip() if banner else None
            published = []
            for path in sorted(self.directory.glob("*.raw")):
                path = _artifact(self.directory, path.name)
                plots = read_raw(path)
                for index, result in enumerate(plots):
                    if not result.signals or any(len(a) == 0 for a in result.signals.values()):
                        raise ValueError(f"empty raw analysis: {path.name}")
                    published.append({"id": f"{path.name}#{index}", "name": result.name,
                                      "analysis_name": path.stem,
                                      "path": path.name, "plot_index": index, "sha256": _digest(path),
                                      "signals": list(result.signals),
                                      "signal_map": {name: name for name in result.signals}})
            if len(published) < data["analyses"]["minimum_plots"]:
                raise ValueError("simulator produced missing or incomplete raw analyses")
            data["results"] = published
            data["status"] = "succeeded"
        except BaseException as error:
            if process is not None:
                _stop(process)
                if data["execution"]:
                    data["execution"]["returncode"] = process.returncode
            if isinstance(error, KeyboardInterrupt):
                data["status"] = "cancelled"
            elif data["status"] not in {"cancelled", "timed_out"}:
                data["status"] = "failed"
            data["diagnostics"].append({"level": "error", "message": str(error) or type(error).__name__})
            data["finished_at"] = _now()
            _atomic(self.directory, data)
            if isinstance(error, (KeyboardInterrupt, SystemExit)):
                raise
            raise SimulationError(str(error), self) from error
        finally:
            try:
                data["finished_at"] = _now()
                _atomic(self.directory, data)
            finally:
                lock.unlink()
        return self
