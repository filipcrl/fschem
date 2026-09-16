# Read-only compiler IR snapshots (version 1)

Python (available now):

```python
parsed = circuit.dump_ir("parsed")
resolved = circuit.dump_ir("resolved")
lowered = circuit.dump_ir("lowered", backend="ngspice",
    config={"parameters": {"R": "10k", "VDD": 3}})
# Reuse an experiment's scalar config, without requiring analyses or PDK binaries:
lowered = sim.dump_ir(backend="vacask")
```

These return detached JSON-compatible dictionaries. Inspection reads schematic and
symbol files; it never executes embedded scripts, simulator programs or GUI code.
There is no IR editing/import API. `lowered` is portable circuit elaboration only:
model identities/bindings are present, external PDK model bodies and Python analysis
commands are not part of this circuit IR. Actual `prepare` additionally validates
technology dependencies and experiment capabilities.

## Rust seam for the integration lane's CLI

```rust
fschem::netlist::dump_ir(
    document: &Document,
    source: &Path,
    paths: LibraryPaths,
    stage: &str,          // parsed | resolved | lowered
    backend: &str,        // ngspice | vacask
    config_json: &str,    // "{}" when absent
) -> Result<String, String>
```

The returned string is pretty JSON only, with no diagnostics mixed into it.
`netlist::lowered_snapshot(&portable::Circuit, source, backend)` returns the same
versioned `serde_json::Value` for an already compiled circuit. Portable
`elaborate(...)` creates typed `Device`, `Terminal`, `Waveform`, scoped-expression
records **before** serialization; `compile(...)` serializes those exact records.
Both native exporters and the resolved inspector reuse `resolve_sheet` connectivity;
inspection does not parse generated decks.

**CLI dispatch is intentionally assigned to integration, not implemented in this
core lane.** Wire the above seam before GUI initialization in `main.rs`. Intended
interface: `fschem --dump-ir parsed|resolved|lowered FILE.sch`, existing backend and
library-path selection, optional `--ir-config FILE.json`, optional explicit output
path. Print JSON only to stdout; errors to stderr with nonzero exit. Reject mutually
exclusive actions/unknown stages/options. Do not call `netlist::write_spice` to write
IR (it is a deck-oriented helper); refuse source/symbol replacement for output.
Add help and CLI regression tests including no GUI/execution, stdout parsing,
malformed config and file output. Core/Python tests already cover the serialization
seam; this is not a claim that CLI flags already exist.

Example lowered configuration (`examples/portable-ir.json`):

```json
{"parameters":{"R":"10k","VDD":3},"local_parameters":{},"exclude":[],"sources":{}}
```

Only these four top-level keys are allowed. Parameter maps contain string expressions
or finite JSON numbers (not booleans/null/arrays). Local map keys are exact instance
paths, `/` for root. Exclusions are exact block selectors. Supplemental source objects
have exactly `kind`, `nodes`, `value`, as described in [portable.md](portable.md).
Malformed JSON, unknown fields and invalid types fail clearly. Nonempty portable
config for parsed/resolved stages errors rather than pretending to apply overrides.
Backend names are validated for all stages. Unsupported/diagnostic-bearing parsed
input, missing symbols, unresolved hierarchy, and unsupported lowered capabilities
return errors; no partial-success circuit snapshots are published.

## Envelope, identities and order

Every snapshot has integer `schema_version: 1`, exact `stage`, and `source` (the
caller-provided source path; Python supplies an absolute path). Objects serialize
with deterministic key order; hierarchical instance/sheet/device arrays are sorted
lexicographically by ID, parameter/net sets are sorted, while **record and pin order
is semantic and retained**. IDs remain stable for unchanged inputs/config, not across
arbitrary source edits. Floating-point effective values are JSON numbers.
Absolute source locations mean snapshots from different checkouts are not byte-identical.

### parsed

- `text`: complete original document text, preserving whitespace/comments.
- `records`: source-order records with `id="record:BYTE_START"`, byte `span=[start,end]`,
  one-based `line`, `raw`, and tagged `kind`.
- Known record fields mirror grammar: header/global properties and tag; instance
  symbol/at/rotation/flip; wire/line/rectangle endpoints (and layer where applicable);
  arc geometry; polygon points; text/scale; embedded records recursively.
- Properties are `{raw, entries:[{key,value}]}`. Entries retain ordering, duplicates
  and bare tokens (`value=null`) rather than collapsing the lossless source into a map.

### resolved

- `sheets`: `{id, source, ports, nets, native_preambles}`. `/` is the root; child IDs
  are instance paths. Ports retain symbol pin order; nets are sorted sheet-local
  connectivity names; S/E preambles remain visible, not executed.
- `instances`: `{id,sheet,source,line,symbol,resolved_symbol,kind,properties,
  symbol_defaults,symbol_metadata,pins}`. Embedded symbols have null resolved paths.
- `pins`: ordered `{order,name,net,properties}`; names may be null for unlabeled pins.
- `ignore_policy` explains portable resolution: backend ignore flags are not applied.
  Annotation/control instances remain visible. Logo/launcher records are not circuit
  components. Opaque/stopped hierarchy errors rather than inventing child structure.

### lowered

- `backend`, `hierarchy_policy`, `requires_ihp_lv_wrappers`.
- `devices`: sorted by human-readable instance-path `id`, with `source`, one-based
  `line` (null for supplemental sources), `symbol` (null for supplemental sources),
  `kind`, `model`, `emitted_name`, ordered `terminals:[{name,node}]`.
- `expressions`: original active attribute expressions (including symbol defaults).
  `parameters`: evaluated effective device scalars. Passives include multiplicity in
  the effective r/c/l; IHP retains independent w/l/ng/m/mm_ok parameters.
- `waveform`: null or tagged `{kind:"dc",value}`, `{kind:"pulse",low,high,delay,rise,
  fall,width,period}`, `{kind:"pwl",points:[[time,value],...]}`.
- `behavior`: null or lowered expression AST. AST nodes use `kind` and `arguments`:
  number (number), name (string, only in non-lowered ASTs), voltage (`[node,node|null]`),
  unary (`[operator,child]`), binary (`[operator,left,right]`). Lowered behavior contains
  no unresolved parameter names. Node names are resolved through the same sheet map.
- `scopes`: sorted `{id,source,effective_parameters,interface_parameters,
  local_definitions,experiment_definitions}` plus `interface_defaults` and
  `instance_overrides` for child scopes. Definition maps contain original expressions;
  effective maps contain numbers. Inapplicable definition maps may be null.
- `signal_map`: canonical `/instance/node` to emitted node. These are circuit node
  identities, not yet raw vector names. The run adapter adds validated canonical-to-raw
  mappings after reading engine results (including explicit ngspice case normalization).

Prepared portable runs store this exact lowered structure as `circuit.ir.json` and
hash it with all inputs. Native runs continue to preserve their original artifacts;
there is no claim to have portable-lowered arbitrary native scripts.

Machine-readable envelope/structural schemas are shipped in
[`schemas/compiler-ir-v1.schema.json`](schemas/compiler-ir-v1.schema.json) and
[`schemas/portable-config-v1.schema.json`](schemas/portable-config-v1.schema.json).
Compiler validation additionally enforces circuit-dependent constraints (scopes,
terminal identity, finite evaluation and supported capabilities); JSON Schema alone
is not electrical or semantic validation. The snapshot schema allows additive fields.
