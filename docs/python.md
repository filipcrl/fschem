# Python experiments and headless API

The installable `fschem` package uses the private PyO3 extension `fschem._core`.
It links the existing Rust library with `default-features = false`: importing it
never initializes a GUI, invokes a simulator, or executes schematic scripts.
Python >=3.10, Rust >=1.90 and NumPy are required to build from this checkout.

```sh
python3 -m venv /tmp/fschem-python-venv
/tmp/fschem-python-venv/bin/python -m pip install 'maturin>=1.9,<2' pytest jsonschema
/tmp/fschem-python-venv/bin/maturin build --interpreter /tmp/fschem-python-venv/bin/python --out /tmp/fschem-python-wheels
/tmp/fschem-python-venv/bin/python -m pip install --force-reinstall /tmp/fschem-python-wheels/*.whl
/tmp/fschem-python-venv/bin/python -m pytest -q
cargo test --no-default-features
```

Alternatively `python -m pip install .` builds/installs the package. The nested
binding crate intentionally has its own lockfile; the legacy native exporters and CLI remain available. Neither virtualenvs nor build outputs belong in commits.

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

`Project(root=".", *, library_paths=(), discover_libraries=True, technology=None)` resolves paths
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
fails using the existing Rust exporter. This native export method does not apply portable experiment semantics, parameter
overrides or technology binding; use `Simulation(..., mode="portable")` for those. Export alone does not stage
include/model files; use native preparation/execution below for relocated runs.

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

## Native simulation lifecycle (POSIX)

See `examples/python_run.py` for an executable divider experiment for either engine.

```python
sim = fschem.Simulation(circuit, mode="native")
prepared = sim.prepare(backend="ngspice")  # Rust netlisting, no process launched
print(prepared.deck)
run = prepared.write("/tmp/my-fresh-run")  # artifacts only, no execution
run = run.execute(timeout=30, env={"OMP_NUM_THREADS": "1"})
# Equivalent: prepared.run(directory="/tmp/another-fresh-run", timeout=30)
# Or: sim.run(backend="ngspice", directory="/tmp/third-fresh-run", timeout=30)

reopened = fschem.Run.open(run.directory)  # never starts a process
print(reopened.status, list(reopened.results))
result = reopened.result("Operating Point")
# ngspice normally lowercases names; VACASK retains native case.
print(result.voltage("vout"))
```

For the native runner, select `Simulation(circuit, mode="native")`. Any non-None
portable override (even an empty mapping) is rejected, never ignored. Analyses and
parameters come from the native deck. The explicit `mode="portable"` alternative
and its additional APIs are documented in [portable.md](portable.md).

`prepare(*, backend)` returns a frozen `PreparedRun` containing the executed deck
and staged text files. `write(directory)` returns a `Run` with status `prepared`.
It accepts a nonexistent or empty directory only, claims it exclusively, and never
reuses results. `PreparedRun.run(*, directory, timeout=None, executable=None,
env=None, cancel=None)` combines write and execute. `Run.execute` has the same
execution options, without `directory`; a prepared run can execute only once.
`executable` is one pathname/program name, not a shell command; no shell is used.
`env` is a mapping of string overrides atop the inherited environment. Neither
cwd nor `os.environ` is mutated. The engine's cwd is always the run directory.
`timeout` is positive finite seconds (default unlimited); `cancel` is an optional
`threading.Event`. Set the event from another thread to cancel synchronous work.

Failures raise `SimulationError`, whose `.run` preserves decks, log and manifest.
Timeout and cancellation are terminal statuses, not successful partial results.
Ctrl-C publishes `cancelled`, cleans up the process group, then re-raises
`KeyboardInterrupt`. TERM followed by KILL cleans up the entire original process
group, including children whose leader already exited. Deliberately detached
processes (new sessions/process groups) are outside this guarantee. Execution is
currently POSIX-only; `UnsupportedPlatform` is raised before `PreparedRun.run`
creates artifacts. Prepare/write/open/raw reading remain cross-platform.

### Native path and script boundaries

Native testbenches are **trusted executable code**, not a sandbox. An explicit
run can execute native shell/control commands and write arbitrary absolute paths;
isolated cwd prevents accidental output mixing, not malicious access. Loading,
netlisting, preparation, writing and reopening never execute a script.

- ngspice: literal `.include`/`.inc`, `.lib FILE SECTION`, control `source`, and
  `osdi`/`pre_osdi` paths are relocated. Quotes/spaces and `.lib SECTION` definition
  blocks are preserved. Included text is recursively snapshotted into `inputs/`;
  relative paths resolve against the containing input, falling back to the source
  directory. OSDI binaries remain absolute external references. Source-local
  `.spiceinit` is snapshotted/relocated as well. Home/system startup follows the
  engine's normal policy; nothing silently disables it. Supply an explicit HOME
  override when a clean startup environment is intended. Dynamic paths containing
  `$`, backticks or braces fail preparation rather than being guessed. Other
  opaque native commands retain their original semantics under the run cwd.
- Straight-line ngspice control analyses get a separate `analysis-NNNN.raw` write
  immediately after each analysis, and quit after the last control block. Dot-card
  analyses use `simulation.raw` (possibly multiple plots). Mixed dot/control
  analyses and control-flow analyses are rejected because faithful result capture
  is not currently implemented. Native exporter/legacy CLI behavior is unchanged.
- VACASK: source-local `.vacaskrc.toml` is parsed and merged into **one**
  `vacask.toml` passed with one `--extra-tomlfile`. All local tables/settings are
  retained. Explicit include/module path arrays are anchored to the source
  directory; the source directory is prepended to both prefix arrays, followed by
  configured prefixes. Unspecified built-in paths remain the engine defaults.
  Engine system/home config is still loaded normally; the merged extra config has
  its normal last-file precedence. Models/includes are external, not frozen.
  Python 3.10 uses `tomli`; serialization uses `tomli-w`.

A zero exit code alone is insufficient: error/fatal/panic and failed/aborted
analysis log diagnostics fail the run; all top-level `*.raw` files must parse via
Rust, contain nonempty signals, and meet the statically visible native analysis
count. Publication is all-or-nothing. Native dynamic/script-generated analyses,
engine-specific diagnostics not recognized by this check, and arbitrary script
output conventions are not a general proof of scientific correctness. Source
and environment dependencies outside the staged inputs are not hermetic.

### Result IDs and reopening

`run.results` is a read-only mapping from `RAW_FILENAME#PLOT_INDEX` to `Result`.
This ID retains duplicate native plot names without overwriting them. VACASK
outputs such as `op1.raw`, `tran1.raw` and `ac1.raw` remain separate files.
`run.result(name)` accepts an exact ID, a unique output basename (`op1`), or a
unique native plot name (`Operating Point`); ambiguity raises `KeyError`.
`Result.name` always retains the native plot name, not the output basename.
Native signals and the manifest's `signal_map` use an identity mapping, intentionally
collision-safe: `v(top:out)` and `v(top.out)` stay distinct. No native aliases/case folding. Portable runs additionally publish validated canonical
node aliases (for example `/OUT`); original native keys remain unchanged.

`Run.open(directory)` accepts prepared, running and terminal manifests. It does
not resume an interrupted process or convert `running` into success. A successful
run is verified against raw SHA-256 hashes and parsed metadata on open and access;
missing/corrupt/changed files fail. Failed runs expose their manifest/diagnostics,
not partial `results`. A process killed with SIGKILL or a machine crash can leave
`running` and a lock behind; automatic recovery/re-execution is intentionally not
supported. Use a new directory. The manifest is inspectable without execution.

## `run.json` schema version 1 (GUI integration contract)

UTF-8 JSON object, atomically replaced after writing/fsyncing a sibling temporary
file created exclusively with a randomized name. Simulator logs are also created
exclusively; pre-existing outputs (including dangling log symlinks) are rejected
before launching. Readers may see a complete earlier lifecycle state, never half-written
JSON. Additive fields may be added; consumers must ignore unknown keys and reject
unsupported `schema_version`. Manifest properties are a detached JSON snapshot.
All timestamps are ISO-8601 UTC strings; unset values are JSON null.

| Field | Type / meaning |
|---|---|
| `schema_version` | integer `1` |
| `status` | `prepared`, `running`, `succeeded`, `failed`, `cancelled`, `timed_out` |
| `backend`, `mode` | `ngspice` or `vacask`; `native` or `portable` |
| `source` | absolute original schematic path |
| `created_at`, `started_at`, `finished_at` | timestamp; latter two nullable |
| `parameters` | native: null; portable: root expression definitions |
| `analyses` | native: `{ "origin": "native_deck", "minimum_plots": positive_integer }`; portable additionally retains named `specifications` with `origin="portable"` |
| `options` | initially `{}`; execution sets `{ "timeout_seconds": number_or_null }` |
| `execution` | null before launch; object described below |
| `artifacts` | `{ "deck": path, "original_deck": "original.deck", "source_snapshot": "source.sch", "log": "simulator.log", "inputs": [path, ...] }` |
| `input_sha256` | object mapping each staged input path to lowercase SHA-256 hex |
| `results` | empty unless succeeded; ordered array described below |
| `diagnostics` | array of `{ "level": "error", "message": string }` |

All artifact paths are relative to the manifest directory and must resolve inside
it (including symlink resolution); source and execution cwd are absolute. `deck`
is `circuit.spice` or `circuit.sim`. `original.deck` is the unmodified Rust native export (or generated portable deck);
`source.sch` is the loaded top-level source snapshot. `inputs` lists every staged
file, including the deck, source, original export and optional startup/config.
The log can be absent before launch. Failed raw files may exist but are **not**
published in `results`. Successful outputs are never replaced by a later attempt.

`execution` fields:

- `argv`: string array, exact subprocess arguments; `cwd`: absolute run directory.
- `executable`: resolved executable pathname when found, otherwise requested name;
  `executable_sha256`: binary/script SHA-256 or null when not found.
- `version`: matching ngspice/VACASK version banner from log, or null when unavailable
  (ngspice batch often has no banner). No extra version-probe process is launched.
- `environment`: only allowlisted effective values: `PATH`, `LANG`, `LC_ALL`,
  `OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS`, `SPICE_LIB_DIR`, `SPICE_EXEC_DIR`,
  `SPICE_SCRIPTS`, `NGSPICE_INPUT_DIR`, when set.
- `environment_override_keys`: sorted explicitly overridden key names; other
  values, including HOME and arbitrary tokens/passwords, are never persisted.
- `environment_policy`: `inherited plus explicit overrides; values allowlisted`.
- `returncode`: integer process return code, negative for signal, or null before
  termination/start failure. Environment provenance is deliberately incomplete to
  avoid leaking secrets; artifacts/native logs themselves may contain user data.

Each `results` item:

```json
{
  "id": "op1.raw#0",
  "name": "Operating Point",
  "analysis_name": "op1",
  "path": "op1.raw",
  "plot_index": 0,
  "sha256": "<64 lowercase hex characters>",
  "signals": ["VOUT", "V1:flow(br)"],
  "signal_map": {"VOUT": "VOUT", "V1:flow(br)": "V1:flow(br)"}
}
```

`name` is exact native plot metadata; `analysis_name` is raw filename without its
extension; `plot_index` is zero-based within that file. Order is filename order,
then native plot order. Viewer navigation should use `id`, display both names,
and open `path` without inferring filename or signal aliases. This schema is an
artifact contract, not permission to execute anything in a manifest.

## Portable compilation and stage inspection

Explicit `mode="portable"` is now available alongside the native runner described
above. See [portable.md](portable.md) for supported devices/waveforms, IHP technology
configuration, parameter scopes, exact native-block migration, op/tran and saves.
`Schematic.dump_ir` and `Simulation.dump_ir` expose deterministic versioned circuit
snapshots; [ir.md](ir.md) specifies their schemas and the `--dump-ir` CLI.
Native mode still rejects portable overrides; its compatibility behavior is unchanged.


## Open runs in the desktop viewer

Build/install the GUI separately (not bundled in the Python wheel):

```sh
cargo install --path . --locked
fschem --open-run /tmp/my-run
# A direct manifest filename is also accepted:
fschem --open-run /tmp/my-run/run.json
fschem --open-text examples/python_portable.py
```

```python
import fschem
process = fschem.viewer.open_run("/tmp/my-run", executable="fschem")
# Returns subprocess.Popen; launch uses an argument list, never a shell.
```

The GUI validates v1, statuses, result IDs/mappings and contained relative paths
(including symlinks); missing/partial files stay visible but unavailable. It offers
original source, generated deck, source snapshot, staged inputs/IR and log navigation,
and opens each named result at its exact plot index in the detached graph window.
An unavailable explicit index remains unavailable on automatic retry, never falling
back to another plot.
Native measurement text is available in the log. An optional additive
`artifacts.measurements` relative text/JSON filename is supported, but the runner
currently does **not** emit a standardized measurements file/API.
Reload the manifest explicitly to see updates. Text logs have their own follow mode.
Ordinary raw opening and schematic graphs discover valid sibling `run.json` signal
maps, display canonical aliases **alongside** native names, and accept canonical
schematic graph node selections. No colon/dot guessing is performed. A custom-named
manifest is navigable, but automatic raw aliases require the standard `run.json` name.

The GUI is an inspector, not the Python integrity/reopening API: it bounds manifests
at 4 MiB, rejects nonregular manifest files before opening (including FIFOs),
and validates graph aliases against parsed plot names and signal lists;
it does not verify recorded SHA-256 hashes. Use `Run.open` for hash-verified results.
Manifest loading is still synchronous: slow filesystem metadata/reads can stall
the UI, and files can change after the regular-file check.
Files on disk can change after inspection; neither viewer is a malicious-filesystem
sandbox. Opening manifests, scripts or sources never runs code. Original schematic
source navigation opens the existing editor; artifact text is read-only.

The optional configured Python script launcher is **deferred**. Run scripts explicitly
from your terminal with your chosen interpreter. No embedded interpreter, automatic
execution on `.py` open, or unsaved-source execution path has been introduced.

Runnable workflows (use fresh output directories and an explicit clean HOME when
host startup configuration needs isolation): `examples/python_native.py`,
`examples/python_run.py`, `examples/python_portable.py`, and
`examples/python_portable_ihp.py --help`. The latter reads external IHP/ADC inputs;
see [portable.md](portable.md) for exact migration and ADC preparation limitations.
