# Portable Python experiments

Portable compilation is an explicit, deliberately bounded alternative to the legacy
native exporters. Python owns experiments; Rust resolves connectivity/hierarchy,
parses expressions, specializes scopes, binds devices and serializes the circuit.
It never translates a generated SPICE deck or evaluates Tcl/Python expressions.
Existing `mode="native"` behavior and exporters remain available.

```python
import fschem
project = fschem.Project(".", discover_libraries=False)
circuit = project.load("examples/portable.sch")
sim = fschem.Simulation(circuit, mode="portable",
    parameters={"R": "10k", "VDD": 3}, temperature_c=27)
sim.op(name="bias", save=["/VOUT"])
sim.tran(name="settling", stop=10e-9, output_step=1e-9, max_step=1e-9)
other = sim.with_parameters(VDD=6)  # distinct experiment; original/source unchanged
prepared = other.prepare(backend="vacask")  # or "ngspice"; no execution
run = prepared.run(directory="/tmp/fresh-portable-run", timeout=30)
print(run.result("bias").voltage("/VOUT"))
```

`op`/`tran` append analyses and return the experiment. `with_parameters(mapping,
**overrides)` returns a copy with an independent analysis list and read-only
parameter mappings. Parameters accept numbers (SI units) or expression strings;
analysis times and temperature accept finite Python numbers only. Temperature is
Celsius, default 27, above absolute zero. Analysis names must be unique ASCII
identifiers beginning with a letter. `max_step` defaults to `output_step`.
`output_step` requests the engine's output/integration interval; adaptive internal
samples may also appear. `ac`/`dc` raise `NotImplementedError`; parameter sweeps can
use `with_parameters(...).op()` or native mode. No implicit mode is selected.

Save lists contain **node paths**, e.g. `/OUT`, `/xadc/SUM1`, not arbitrary simulator
expressions. Empty saves retain the engine default set. Both engines support
per-analysis node subsets; transient time is retained. Ground and duplicate/aliased
save nodes are rejected. `Result.signals` retains native names;
`Result.signal_map` adds known canonical path mappings and `voltage("/path/node")`
uses them. Native-mode mappings remain identity-only. There is no colon/dot guessing.

## Supported circuit subset

- R/C/L with positive scalar values and multiplicity: effective R/L = value/m,
  effective C = value*m. Independent `vsource`/`isource` DC, exactly seven-argument
  `PULSE(low high delay rise fall width period)`, and strictly increasing-time
  `PWL(time value ...)` with at least two points. Arguments accept scoped expressions.
- Four-terminal VCVS (`vcvs`, scalar gain).
- Behavioral voltage sources (`source`, `VAR=V`, unity `m`) with arithmetic and
  `v(node)`/`v(nodeA,nodeB)` references to literal nodes in their own sheet. This covers
  ADC gated clocks such as `v(PHI1_SRC)*v(CLK_EN)`. Dynamic powers, functions, current
  accessors, ternaries, comparisons and arbitrary native code are unsupported.
- Schematic hierarchy is specialized **per instance**, then flattened. Interface
  pin order and schematic port membership are validated. Internal node/device names
  use collision-free path hex encoding; `circuit.ir.json` retains human-readable paths.
- IHP `sg13_lv_nmos`/`sg13_lv_pmos` symbols bind only to matching existing subcircuit
  wrappers, never bare PSP devices. Terminal matching is by unique D/G/S/B names,
  reordered D/G/S/B in both engines. Resolve `w,l,ng,m,mm_ok` from symbol template then
  instance properties once. `ng` (positive integer) and `m` remain independent;
  symbol `mm_ok=1` is passed explicitly rather than adopting wrapper default 0.
  No per-instance `vacask_format` is required. Extra MOS geometry/model parameters
  require a further tested binding and currently error.

Scalar expressions support numeric SI values, engineering suffixes
`t,g,meg,k,m,u,n,p,f` (case-insensitive), names, parentheses, outer braces,
`+ - * / ^` and unary signs. Names are case-sensitive; values are evaluated to finite
doubles before emission. No eval, Tcl, unit-tail guessing (`1nF`), functions or
cross-instance parameter references. Undefined names, cycles and non-finite results
error. Expressions are bounded to 16 KiB, 512 subexpressions and 128 recursive levels; hierarchy to 32.

Unknown devices/properties, custom `device_model`, opaque/stopped hierarchy,
backend-specific ignore/short flags and unknown/ambiguous scopes fail before launch.
Native formatting annotations on otherwise supported symbols are not portable
semantics: portable bindings use the structured attributes above, not
`vacask_format`, `vacask_params` or native template expansion. Presentation properties
are allowed explicitly. Source `value`, not a backend annotation, defines stimulus.

## Parameter scope and migration

There are three separate concepts:

1. `parameters={...}` declares the root experiment's scalar environment.
2. A subcircuit **symbol template** (excluding `name`) declares its interface defaults.
   Instance properties override only declared interface keys. Override expressions
   evaluate in the caller scope; dependent defaults evaluate against effective
   overrides in the child's own interface scope. Child sheets do not inherit arbitrary
   caller locals. Interface names/defaults/overrides/effective values remain in IR.
3. `local_parameters={"/xadc": {"CUNIT": "10f", ...}}` declares private sheet locals;
   `/` denotes the root. Locals may reference that sheet's interface and other locals,
   but cannot redeclare an interface/experiment parameter. Unknown paths error.

A native `netlist_commands` component **always conflicts**, even if its VACASK
annotation is empty or marked `only_toplevel`. Opt out explicitly using
`exclude=["/xinv/PARAMS"]`, or `"/absolute/path/inv.sch#PARAMS"` to select that one
named block in every instance of that exact source file. Unmatched exclusions error.
There are no wildcards or blanket ignore switch. Nonempty S/E global preambles must
be migrated in the source; they are not silently discarded and have no exclusion key.

For the external ihp-adc example:

- `inv.sym` already declares `wp,wn,l`; `inv.sch#PARAMS` repeats these as `.param`.
  Exclude that block; **do not** migrate it to locals (that would shadow the interface).
- `tg.sym` declares `inv_wp,inv_wn,inv_l` (and `wp,wn`); exclude `tg.sch#PARAMS` for
  the same reason. The actual transistor widths in the current `tg.sch` are fixed
  instance properties, not falsely treated as uses of `wp/wn`.
- `adc.sym` declares `inv_wp,inv_wn,inv_l`. Exclude `adc.sch#CAP_SCALING`, preserve
  those interface parameters, and migrate only `S1,S2,CUNIT,CS1_BASE,CI1_BASE,
  CS2_BASE,CI2_BASE,CFFIN_BASE,CFFY1_BASE,CFFY2_BASE` to `/xadc` locals.
- Exclude root `/MODELS` and `/CONTROL`. Move model selection to `Project(technology=...)`,
  root testbench constants to `parameters`, and analyses to Python. Native simulator
  tuning/options are **not** auto-translated: this example uses portable defaults.
- `/MODELS` also contained **VCLKEN**! Explicitly recreate it with the narrow source
  migration API below; omitting it causes undefined behavioral-node rejection.

```python
sources = {"/VCLKEN": {
    "kind": "voltage",                 # or "current"
    "nodes": ["/CLK_EN", "/0"],         # root only; /0 is circuit ground
    "value": "PWL(0 1 {TSTOP} 1 {TSTOP+TEDGE} 0)",
}}
```

Pass `sources=sources` to `Simulation`. These are structured root testbench sources,
not raw deck fragments or a general circuit-construction API. The same Rust waveform
parser and root environment serve drawn/supplemental sources. Duplicate names
(case-insensitive, including drawn components), unsupported scopes/kinds, wrong
terminal counts and undefined expressions error. New root nodes are permitted.
Source specifications, effective waveform records and exclusions are recorded.

## Technology configuration and examples

```python
pdk = fschem.IHP("/absolute/path/IHP-Open-PDK")
project = fschem.Project("/absolute/path/ihp-adc/schem", technology=pdk,
    discover_libraries=False,
    library_paths=["/absolute/path/IHP-Open-PDK/ihp-sg13g2/libs.tech/xschem",
                   "/absolute/path/xschem/xschem_library/devices"])
```

`corners="tt"` (default when an IHP device is present), `ss`, `ff`, `sf`, `fs` select
existing `cornerMOSlv.lib` sections. Stochastic corners and other IHP families are
unsupported. Configuration refers to the installed wrappers and PSP103/PSP103_NQS
OSDI modules without copying or modifying private model files. VACASK's required
`swsoa=0` is explicit. No universal model converter is involved. Missing dependencies
error during preparation. Binary engine/module ABI compatibility can only be tested
at execution. ngspice OSDI paths with whitespace are currently rejected.

Portable preparation does **not** import source-local native startup/config scripts.
It writes only its explicit technology `.spiceinit`/VACASK paths. Home/system engine
startup is still inherited: pass an explicit clean HOME for isolation where needed;
there is no sandbox or automatic global environment mutation. External PDK files are
not frozen/hashed recursively. Native mode retains its existing local-config policy.

Runnable examples:

```sh
python examples/python_portable.py --backend vacask --directory /tmp/new-divider
python examples/python_portable_ihp.py --backend ngspice --circuit inverter \
  --pdk /absolute/IHP-Open-PDK --schematics /absolute/ihp-adc/schem \
  --devices /absolute/xschem/xschem_library/devices --directory /tmp/new-inverter
# Same flags, --circuit adc --prepare-only: complete ADC testbench deck preparation.
```

The IHP example is validated with both real engines for inverter DC bias and transient
inversion. ADC validation covers complete preparation (including behavioral clocks,
VCVS, capacitor scaling, hierarchy and VCLKEN), **not** transient convergence, ADC
accuracy/noise or agreement with native simulator-specific tolerances.

## Artifact additions (run.json schema version remains 1)

Portable runs set `mode="portable"`, `parameters` to the root expression mapping,
and `analyses.origin="portable"` with `specifications` (name/kind/save and tran times).
The additive `portable` object holds root definitions, effective scoped parameter
records, local definitions, excluded block selectors, supplemental sources, corners,
temperature, technology description and canonical-to-emitted node map.
`artifacts.inputs` includes hashed `circuit.ir.json`: the exact immutable elaborated
snapshot used to emit this deck. `original.deck` is the generated portable deck,
not a legacy native export. Source `.sch` is never modified. Save options/specifications
are copied at prepare time; later analysis additions do not change a prepared run.
Successful portable execution requires each expected named raw file/plot kind, not
merely a minimum count. Existing native schema fields/lifecycle remain unchanged.

For machine-readable stage inspection and CLI integration see [ir.md](ir.md).
