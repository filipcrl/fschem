# Headless Python API (stage 1)

The installable `fschem` package uses the private PyO3 extension `fschem._core`.
It links the existing Rust library with `default-features = false`: importing it
never initializes a GUI, invokes a simulator, or executes schematic scripts.
Python >=3.10, Rust >=1.90 and NumPy are required to build from this checkout.

```sh
python3 -m venv /tmp/fschem-python-venv
/tmp/fschem-python-venv/bin/python -m pip install 'maturin>=1.9,<2' pytest numpy
/tmp/fschem-python-venv/bin/maturin build --interpreter /tmp/fschem-python-venv/bin/python --out /tmp/fschem-python-wheels
/tmp/fschem-python-venv/bin/python -m pip install --force-reinstall --no-deps /tmp/fschem-python-wheels/*.whl
/tmp/fschem-python-venv/bin/python -m pytest -q
cargo test --no-default-features
```

Alternatively `python -m pip install .` builds/installs the package. The nested
binding crate intentionally has its own lockfile; the root crate and legacy CLI
are unchanged. Neither virtualenvs nor build outputs belong in commits.

## Load, inspect and generate native decks

```python
import fschem

project = fschem.Project(root=".", library_paths=[], discover_libraries=False)
circuit = project.load("examples/spice.sch")
print(circuit.path)             # absolute source path
print(circuit.source)           # original lossless loaded text
for diagnostic in circuit.check(backend="ngspice"):
    print(diagnostic.file, diagnostic.line, diagnostic.message)
deck = circuit.netlist(backend="ngspice")  # returns text, writes nothing
```

`Project(root=".", *, library_paths=(), discover_libraries=True)` resolves paths
against an absolute root captured at construction; it never changes cwd or the
process environment. Library discovery defaults to the legacy Rust discovery
(environment/system Xschem paths). Set it false for explicit-only library roots.
Source-relative symbols continue to work in either mode.

`Project.load(path)` returns a `Schematic`. Its top-level source is a snapshot;
referenced symbols/hierarchy files are read by the native loader/exporter and are
not frozen across exports. Reload to pick up top-level source changes.
`diagnostics` and `check()` return tuples of frozen `Diagnostic(file, line,
message)` objects. Lines are one-based; zero means a file-level diagnostic.
These are loader diagnostics, not an electrical rule checker or severity-ranked
lint. `check(backend=...)` additionally generates/discards a native deck to check
export validity, raising `ValueError` on failure. An empty diagnostic tuple is
not a guarantee of simulator compatibility.

`Schematic.netlist(*, backend="ngspice")` supports exactly `ngspice` (legacy
SPICE exporter) and `vacask` (native VACASK exporter). Native backend-specific
commands and analyses are retained, not translated. Unsupported export syntax
fails using the existing Rust exporter. No portable experiment semantics,
parameter overrides, technology binding, subprocess runner or run-manifest
schema are introduced at this stage. Export does not stage include/model files;
callers must preserve their native path context until the preparation API lands.

Load, export and raw-reader failures currently raise `ValueError` with Rust
context, including filesystem failures. The private extension and its tuple
transfer format are not a public schema. Use only the Python wrappers.

## NumPy raw results

```python
plots = fschem.read_raw("simulation.raw")
for result in plots:
    print(result.name, tuple(result.signals))
    # Transient plots:
    # time = result.time
    # output = result.voltage("VOUT")
```

`read_raw(path)` returns a tuple of `Result`, one per concatenated native plot.
It reuses the Rust ASCII/native-endian binary, real/complex raw parser (ngspice
and compatible VACASK output), with its 256 MiB / 16-million-sample limits.
Arrays are owned copies (`float64` or `complex128`), not zero-copy or lazy data.
Transfer currently uses intermediate Python lists, so peak memory exceeds raw
file size. Signal/kind mappings are read-only; the arrays themselves are mutable.

- `result.title`, `result.name`: exact native metadata.
- `result.signals`, `result.kinds`: exact native signal-name mappings.
- `result[signal]`: exact signal lookup; unknown signals raise `KeyError`.
- `result.scale_name`, `result.scale`: native first-variable scale, or `None`
  for operating-point plots (the existing Rust convention).
- `result.time` / `result.frequency`: real scale array if its kind matches,
  otherwise `ValueError`.
- `result.voltage(node)`: `v(node)` if present, otherwise exact `node` lookup.
  No case folding or `:`/`.` rewriting; use `result["v(node)"]` to disambiguate.

Duplicate raw signal names fail rather than silently losing a column. Duplicate
plot names remain separate entries. No canonical signal aliases are invented.
See `examples/python_native.py` for a runnable export/raw-inspection example.
