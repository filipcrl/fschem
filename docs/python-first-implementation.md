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

One writer at a time in this checkout. Reviewers are read-only. Each implementation stage reads this plan and preceding stage's report, tests its seam, commits its changes frequently, and reports concrete commands, commit IDs, API/schema decisions and residual risks. Ask the supervisor before destructive actions or changing architecture/scope. Infrastructure failure is a blocker, not permission to change execution protocol.
