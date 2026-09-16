# Python-first simulation implementation

## Approved direction

Python owns experiments, orchestration, sweeps and measurements. Rust owns lossless Xschem parsing, hierarchy/connectivity, expressions, technology bindings, native deck generation and raw-file reading. The GUI remains a schematic editor/viewer and graph viewer, with a simple read-only text viewer and run-artifact navigation. Importing Python must not initialize a GUI. Do not embed Python or automatically execute opened scripts.

Existing source at commit 3699e21 was checkpointed separately before this change. Do not modify ../ihp-adc or ../IHP-Open-PDK. Use their contents only as integration inputs with output in temporary directories. Preserve the legacy native exporters and CLI during the migration; label compatibility modes explicitly instead of silently translating arbitrary control scripts.

## Delivery sequence

1. **Native Python bindings:** PyO3/maturin private `fschem._core`, ordinary Python public API, headless load/check/netlist/raw reading, packaging and Python tests. Reuse the Rust library, not executable shell wrappers or duplicate Python parsers.
2. **Python orchestration:** Project, Simulation, PreparedRun, Run and NumPy Result APIs; ngspice/VACASK process adapters; native preparation/execution; isolated run directories, timeout/cancel/error handling, structured manifests and reopening. Native mode must not pretend to implement portable parameter overrides.
3. **Portable preparation:** shared circuit representation, expression/source lowering, portable experiment specification, explicit parameter scopes/overrides, IHP LV MOS binding to the existing wrappers and corner dependencies. Portable op/tran and preferably ac/dc support, fail-closed unsupported capabilities and native-code conflicts. Reuse connectivity/hierarchy rather than regex rewriting generated decks. Include portable inverter and ADC-oriented examples/fixtures. Add further support only when covered by tests.
4. **Viewer artifacts:** reusable read-only text viewer; opening scripts/decks/logs/model/config text; search, line numbers, copy, goto, reload, wrap and follow where practical; bounded/background reads and responsiveness; `--open-run` manifest navigation to source, analyses, deck and log. Keep existing detached graph windows initially. No implicit script execution. Opening text should not invoke schematic actions.
5. **Integration/documentation:** validate public API end-to-end, add executable examples, implement Python viewer launch, CLI headless Python commands if useful, opt-in configured script launch from GUI only with explicit interpreter and save-before-run protection. Test/document stable supported API and exact limitations. Existing-window IPC, embedded Python, zero-copy/huge datasets, advanced arbitrary derived-array graph storage and full Python source editing are later work, not prerequisites for this first complete vertical slice.
6. **Independent review and fixes:** review Python/compiler/process correctness and GUI/artifact/compatibility independently, address concrete defects, rerun focused/full tests, frequent focused commits.

## API intent

Target a natural API like `project = fschem.Project(root=..., technology=...)`, `circuit = project.load(...)`, `sim = fschem.Simulation(circuit, parameters=..., corners=..., temperature_c=...)`, `sim.tran(name=..., stop=..., output_step=..., max_step=..., save=...)`, `prepared = sim.prepare(backend=...)`, `run = prepared.run(directory=..., timeout=...)`, `result = run.result(name)`, `result.time` and `result.voltage(...)` as NumPy arrays. `sim.run(...)` is convenience, `with_parameters` does not modify sources, and `Run.open(...)` reopens disk artifacts. Settle concrete signatures consistently in the first stages, document and extend rather than creating parallel APIs.

Native and portable experiments must be explicit. Unknown portable devices, conflicting native analyses, ambiguous parameter overrides and unsupported backend capabilities error before launch. Preserve public interface parameters/defaults, local parameters and experiment parameters distinctly. Use symbolic expression lowering, not eval or text substitution. Technology mapping identifies the IHP symbol/model and maps named terminals D/G/S/B; both backends call existing sg13_lv_nmos/pmos subcircuit wrappers, not bare PSP devices. Resolve symbol defaults once (notably mm_ok=1 vs wrapper default 0), preserve ng and m independently, and do not modify the PDK.

## Artifact contract

Use a versioned generated `run.json`, atomic updates, and immutable/fresh run directories (refuse existing nonempty destination by default). Record schema version, lifecycle status, backend, source, effective parameters/analyses/options, commands, executable/version where available, explicit relevant environment (not arbitrary secrets), artifact paths, results and diagnostics. Prefer relative artifact paths inside the run; original source may be absolute. Establish the exact schema in stage 2 and consume it unchanged in stage 4 (additive extensions are fine). Completed raw results are published only after parsing and validation, and existing successful results are never overwritten by failure. Keep logs and decks on failure. Retain analysis names and native signal names plus canonical mappings; do not blindly equate all ':' and '.' identifiers.

## Validation

- Rust unit/integration tests and existing CLI tests remain passing.
- Python package builds/imports without GUI, tests run in an isolated venv, wheel build exercised.
- Native divider examples work through both real engines where installed; fake executables test errors, bad/absent raw output, startup diagnostics, timeouts, cancellation and concurrent jobs.
- No global chdir or environment mutation and no shell interpolation. Include/model paths work independently of output directory; VACASK local config must be merged intentionally (the installed version may honor only the last --extra-tomlfile).
- Portable primitives and source waveforms produce the same intent on both engines; technology fixtures test parameters, pin order, ng/m and symbol defaults. Don't ship private PDK files in fixtures.
- Tests cover expression scope and unsupported syntax/native conflict rejection.
- GUI tests cover type dispatch, text state/search/reload and invalid/partial manifests without requiring display where possible; graphical smoke tests when feasible.
- Run an IHP inverter integration and prepare the ADC hierarchy when portable support permits; report exact gaps instead of claiming unsupported features.
- Preserve pre-existing source files, never push, and make small meaningful commits at working milestones.

## Working rules

One writer at a time per checkout/worktree. Reviewers are read-only. The user requested parallel implementation after stage 1; the initial sequential workflow was stopped at a clean committed checkpoint (665c68c and 6374cae). Continue with isolated lanes below, then a serial integration gate. Each implementation stage reads this plan and preceding stage's report, tests its seam, commits its changes frequently, and reports concrete commands, commit IDs, API/schema decisions and residual risks. Ask the supervisor before destructive actions or changing architecture/scope. Infrastructure failure is a blocker, not permission to change execution protocol.

## Parallel lane board

| Lane | Checkout / branch | Ownership | Gate / handoff | Independence |
|---|---|---|---|---|
| Python/compiler | `../fschem-worktrees/python-core`, `feature/python-sim-core` | Python API/runners, Rust compiler/technology/bindings, non-GUI tests and Python docs | Runner and portable compiler tests, frequent commits; integration merges named branch | Does not change GUI files or CLI dispatch |
| Text viewer | `../fschem-worktrees/text-viewer`, `feature/text-viewer` | `src/viewer.rs`, `src/main.rs`, `src/macos_menu.rs`, new text-viewer module/tests | Text and GUI regression tests, frequent commits; integration merges named branch | No dependency on run manifest or Python API; run-artifact UI deferred to integration |
| Integration | Main checkout, `master`, only after both writers finish | Merge both branches, resolve conflicts, run manifest/UI/Python launcher integration and docs | End-to-end tests then fresh parallel reviews | Sole writer after dependency barrier |

Both lane worktrees are persistent local Git worktrees, allocated by the parent before launch. No child may edit another lane checkout. Read-only integration inputs remain at `/Users/filipcorlan/work/ihp-adc` and `/Users/filipcorlan/work/IHP-Open-PDK` (their relative locations differ in lane worktrees). Keep worktrees until parent acceptance; no automatic branch deletion. All child agents use medium thinking. Reviews run concurrently after integration and precede a single fix writer.

## Approved IR inspection addition (user steering during portable lane)

Add agent-friendly, read-only deterministic JSON snapshots for `parsed`, `resolved`
and `lowered` stages. Each has `schema_version`, `stage`, `source`, stable IDs and
source locations, ordered pins/terminals, identity and parameter/net information.
Parsed inspection preserves lossless records; resolved inspection uses shared
connectivity/hierarchy; lowered inspection exposes the actual elaborated circuit
before deck serialization, not Debug output or a JSON-wrapped deck. Invalid
options and unsupported stages fail clearly; inspection never launches engines.
The core lane owns Rust/Python inspection, schema documentation and tests. The
integration lane owns CLI dispatch in `main.rs`: `--dump-ir STAGE FILE.sch`,
optional JSON portable preparation config, existing backend/library selection,
JSON-only stdout (or explicit output file), diagnostics on stderr/nonzero status,
no GUI initialization, help and CLI regression tests.

## Integration progress (completed implementation; review fixes validated)

Both finished lanes were merged into master with normal non-fast-forward merges;
all lane commits and persistent worktrees are retained. Integration now provides:

- `--dump-ir` CLI with backend/library selection, strict portable JSON config,
  JSON-only stdout and fresh-file-only output (cannot replace source/config/symbols).
- `--open-run DIRECTORY|MANIFEST`, reloadable read-only run navigation with status,
  original source, analyses, exact result plot selection, decks/log/inputs/IR and
  optional measurement text. V1/status/path checks reject invalid/escaping paths;
  missing and prepared/failed artifacts are inspectable without execution.
- Headless `fschem.viewer.open_run(path, *, executable="fschem")`, launching an
  external GUI with an argument list. The wheel intentionally excludes the GUI.
- Standard sibling manifests supply validated canonical aliases to ordinary raw
  opening and existing schematic graphs; native names remain visible and intact.
  Detached graph windows remain unchanged in architecture.
- Updated installation, wheel, IR, portable/native and ADC-oriented documentation.

Integration validation on macOS arm64 / CPython 3.12:

- Installed-wheel Python suite: **79 passed**, with explicit external IHP/ADC/device
  paths. Wheel built with maturin and imported from site-packages, not source.
- `cargo test --all-targets`: **92 passed, 7 ignored**.
- `cargo test --no-default-features --lib`: **31 passed, 4 ignored**.
- Both full `cargo clippy --all-targets -- -D warnings` and headless-library clippy
  passed after narrow GUI lint corrections (no unrelated mass formatting).
- Real native and portable divider runs passed on ngspice and VACASK; portable
  IHP inverter executions passed both engines, and full ADC testbench preparation
  passed both. External inputs were read-only; outputs are under
  `/tmp/fschem-integration.aQA6Nd`.
- GUI smoke: schematic demo, explicit Python text, and succeeded run artifacts.
  Run-view screenshot inspected; installed Python launcher also started the GUI
  successfully and its returned process was explicitly terminated after checking.

Remaining work/limits:

- Independent reviews identified five concrete defects; all are fixed below. Parent
  acceptance of the fix commits remains required.
- Optional GUI configured Python script execution is **deferred**, not silently
  enabled. Choose the interpreter and run scripts explicitly from the terminal.
- No standardized measurement persistence API was invented; native measurement text
  remains in simulator logs. Optional `artifacts.measurements` navigation is additive.
- GUI inspection does not hash-verify files (use Python `Run.open`); aliases require
  matching parsed names/signals and a standard sibling `run.json`. Manifest reads
  are bounded at 4 MiB. Source/graph/text navigation has automated/state/render
  coverage, not a complete manual mouse/keyboard workflow certification.
- Portable AC/DC/noise, full ADC convergence/accuracy/noise and native-tolerance
  equivalence remain unsupported/unverified as documented in `portable.md`.
- Execution remains POSIX-only; Python 3.10 and other OS GUI runtimes were not tested.
  Native code is trusted, not sandboxed; external models/modules are not hermetic.


## Independent review fixes (single-writer acceptance stage)

All five reported findings were current, not stale: flattened/root node collisions,
backend-specific flags on labels, symlink redirection of runner outputs, explicit
plot-index fallback on retries, and FIFO manifests blocking at open. Focused fixes
reject ambiguous node names and flagged labels before emission, exclusively create
logs/randomized manifest temporary files, retain explicit plot selection on retries,
and reject nonregular manifests before opening. No public API or v1 schema changed.

Regression/acceptance validation on macOS arm64 / CPython 3.12:

- Release maturin wheel rebuilt and force-installed into `/tmp/fschem-core-venv`;
  imported from site-packages, not the checkout. Full Python suite **89 passed**
  with explicit absolute IHP/ADC/Xschem paths. Includes real native/portable dividers
  and IHP inverter runs on both engines, plus ADC preparation on both engines.
- Focused portable/runner suite: **58 passed**, including eight compiler and two
  symlink regressions. No simulator launches for rejected static output symlinks.
- `cargo test --all-targets`: **94 passed, 7 ignored**; includes invalid-selection
  automatic retry and writerless FIFO regressions.
- `cargo test --no-default-features --lib`: **31 passed, 4 ignored**.
- `cargo clippy --all-targets -- -D warnings`: passed.

FIFO rejection addresses the concrete blocking reproduction. Background manifest
loading on slow filesystems is not implemented; synchronous metadata/read calls and
filesystem replacement races remain explicit viewer limitations, not sandboxing
claims. No new graphical/manual interaction smoke was performed in this fix stage;
prior integration smoke and current headless GUI state/render tests are distinct.
Portable AC/DC/noise, full ADC convergence, configured script launching, standardized
measurement persistence, non-POSIX execution and hermetic dependencies remain out
of scope. All lane branches/worktrees and external inputs were preserved.
