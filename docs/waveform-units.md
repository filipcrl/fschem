# Waveform types and calculated traces

Raw variables retain `native_kind` exactly as supplied. Effective `kind` and
`unit` are separate. Time/frequency axes, `v(node)`, `i(source)`, `#branch` and
VACASK `:flow(br)` conventions resolve to seconds/Hz/volts/amps. A wholly untyped
VACASK-style dataset containing `:flow(...)` vectors uses voltage units for bare
node names. Internal `:noi`/non-branch flow quantities remain unknown. Arbitrary
untyped vectors from an unidentified producer also remain unknown (`?`), rather
than displaying `notype` as if it were a physical unit. Inference never changes
signal names or values; original metadata remains available for inspection.

The waveform viewer's **Expression / Add trace** evaluates bounded arithmetic,
not Python, Tcl or simulator code. Examples:

```
v(OUT)/v(IN)          # V/V
v(OUT)/i(VDD)         # Ω
v(OUT)*i(VDD)         # W
deriv(v(OUT))         # V/s for a time plot
abs(v(OUT))           # V
```

Enter the expression without the explanatory comment. Supported syntax is
finite numeric constants, `+ - * /`, parentheses, `abs(...)`, `deriv(...)`,
`v(name)`, `i(name)`, bare vector names and backtick-quoted exact native names
(e.g. `` `VDD:flow(br)` ``). Exact names win; a case-insensitive match is accepted
only when unambiguous. Constants are dimensionless. Incompatible known units in
addition/subtraction are rejected; unknown inputs propagate unknown units.
Nested derivatives are unsupported. `deriv` uses endpoint/central secants over
the plot axis. Zero denominators/nonfinite inputs yield nonfinite samples,
rendered as gaps. This is not a symbolic mathematics or general SPICE evaluator.
Calculated vectors are in-memory additions; the original raw file is unchanged.
When the window is linked to a schematic graph, its declarative expressions are
saved with the graph configuration and recalculated on reopening. At most 64
expressions and 16 million total raw/calculated samples are accepted per display.

Python exposes the same evaluator and type model:

```python
result = run.result("conversion")
print(result.units, result.kinds, result.native_kinds)
r = result.calculate("v(/OUT)/i(VDD)") # canonical voltage aliases supported
print(r.unit, r.kind, r.values)        # Ω, resistance, NumPy array
```

`read_raw()` also provides the metadata mappings. A stored vector named `gain`
with no formula/provenance cannot have its dimensions reconstructed reliably;
use an explicit calculated expression instead of guessing.
