"""Portable experiment preparation; circuit and expression lowering live in Rust."""
import json
import math
import re
import tomli_w


def number(value, label, *, positive=False):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"{label} must be a finite number in SI units")
    if positive and value <= 0:
        raise ValueError(f"{label} must be positive")
    return value


def name(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", value):
        raise ValueError("analysis name must be an ASCII identifier starting with a letter")
    return value


def expressions(values):
    result = {}
    for key, value in (values or {}).items():
        if not isinstance(key, str):
            raise ValueError("parameter names must be strings")
        if not isinstance(value, str):
            value = str(number(value, key))
        result[key] = value
    return result


def prepare(sim, backend):
    from .simulation import PreparedRun
    from types import MappingProxyType
    if backend not in {"ngspice", "vacask"}:
        raise ValueError("unsupported portable backend")
    if not sim._analyses:
        raise ValueError("portable simulation requires at least one analysis")
    body, ihp, signals = sim.circuit._native.portable(
        backend, dict(sim.parameters), {k: dict(v) for k, v in sim.local_parameters.items()}, set(sim.exclude), dict(sim.sources))
    files = {}
    paths = {}
    prefix = ""
    if ihp:
        if sim.circuit.project.technology is None:
            raise ValueError("IHP devices require Project(technology=IHP(root))")
        prefix, files, paths = sim.circuit.project.technology.dependencies(backend, sim.corners or "tt")
    elif sim.corners is not None:
        raise ValueError("corners require a technology-bound circuit")
    # Public saves use exact canonical paths, not backend colon/dot guesses.
    def saves(analysis):
        result = []
        for key in analysis["save"]:
            if key not in signals:
                raise ValueError(f"unknown save node {key!r}; use canonical /instance/node paths")
            node = signals[key]
            result.append(f"v({node})")
        return result
    analyses = []
    for analysis in sim._analyses:
        saved = saves(analysis)
        if backend == "ngspice":
            command = "op" if analysis["kind"] == "op" else (
                f'tran {analysis["output_step"]:e} {analysis["stop"]:e} 0 {analysis["max_step"]:e}')
            analyses.append(f'{command}\nwrite {analysis["name"]}.raw ' + (" ".join(saved) if saved else "all"))
        else:
            command = "op" if analysis["kind"] == "op" else (
                f'tran step={analysis["output_step"]:e} stop={analysis["stop"]:e} maxstep={analysis["max_step"]:e}')
            # VACASK save is a circuit-level directive, so per-analysis subsets are
            # rejected rather than silently conflated.
            if saved:
                raise ValueError("portable VACASK save subsets unsupported; omit save to retain all nodes")
            analyses.append(f'analysis {analysis["name"]} {command}')
    if backend == "ngspice":
        deck = (f'* fschem portable\n{prefix}{body}.temp {sim.temperature_c:e}\n.control\n'
                + '\n'.join(analyses) + '\n.endc\n.end\n')
        filename = "circuit.spice"
    else:
        deck = (f'fschem portable\n{prefix}{body}control\nabort always\n'
                f'options temp={sim.temperature_c:e}\n' + '\n'.join(analyses) + '\nendc\n')
        filename = "circuit.sim"
        files["vacask.toml"] = tomli_w.dumps({"Paths": paths})
    files.update({filename: deck, "original.deck": deck, "source.sch": sim.circuit.source})
    metadata = {"parameters": dict(sim.parameters), "analyses": list(sim._analyses),
                "local_parameters": {k: dict(v) for k, v in sim.local_parameters.items()},
                "sources": {k: {"kind": v[0], "nodes": v[1], "value": v[2]} for k, v in sim.sources.items()},
                "excluded_native_blocks": sorted(sim.exclude), "corners": sim.corners,
                "temperature_c": sim.temperature_c, "signal_map": signals,
                "technology": str(sim.circuit.project.technology) if ihp else None}
    return PreparedRun(backend, sim.circuit.path, filename, MappingProxyType(files),
                       len(analyses), "portable", json.dumps(metadata, allow_nan=False))
