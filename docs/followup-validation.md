# Follow-up integration evidence

Implemented: display-only field elision and simulator toolbar selector; project
`.fschem.toml` discovery in CLI/Python; native/effective waveform types and derived
units; E-to-open linked graph configuration with undo, stacked embedded graphs,
calculated-trace persistence and non-interfering embedded hover readouts.

## Parent verification

- `cargo test --locked --all-targets`: **111 passed, 7 ignored**, zero failures.
  Includes linked-owner/history/stale-update tests, delayed trace preparation,
  expression configuration round trips/removal, embedded hover readout/drag
  suppression, scoped PDK variables and CLI `-L` precedence.
- Rebuilt Python wheel with `maturin build`, installed into the isolated
  `/tmp/fschem-parent-venv`; **93 Python tests passed**, including real IHP inverter
  runs on both engines and archived native ADC preparation.
- ihp-adc: **23 tests passed**. Automatic CLI/Python configuration checked with
  PDK_ROOT/PDK unset; 15 source files/852 records pass lossless parser checks.
- Fresh migrated ADC-project suite: **12 real simulations completed**, ten cases
  pass measurements and two TG cases retain their original hold-threshold failure.
  Evidence: `../ihp-adc/runs/validation-9kzxiker/validation.json`. See that project's
  migration document for physical measurements and ADC/noise limitations.
- Native stacked-graph smoke passes; screenshot inspected at
  `/tmp/fschem-final-stacked.png`. Separate voltage/current panels show correct
  electrical units. ADC GUI also renders with automatically resolved libraries and
  selected VACASK, but its strict smoke gate exits nonzero for the known unresolved
  display-attribute/Tcl warning (64 labels); `/tmp/fschem-final-adc.png` captures
  this, with raw graphs still loading at the early screenshot. This is not claimed
  as a clean ADC GUI smoke or full OS-level interactive acceptance.
- ADC preexisting tracked diffs for `adc.py`, `notebook.ipynb`, `schem/ADC.md`, and
  `schem/simulations/tb.spice` compared byte-for-byte against the pre-migration patch:
  all unchanged. Old datasets/studies and the PDK were not rewritten.

## Review

The independent medium-thinking graph review found asynchronous auto-fit,
undo-selection and movement-blocked synchronization defects. The fix commit
`dec6be2` (integrated as `3c77950`) addresses all three with regressions, independently
rerun by the parent. The ADC review found construction failures escaping the
validator loop; `98f3669` fixes it with continue-and-report tests. Parent integration
adds expression synchronization and hover coverage, with the final suites above.

The worker worktrees are retained, no branches pushed, and generated run evidence
is ignored rather than committed. See [linked graphs](linked-graphs.md),
[waveform units](waveform-units.md), and [project config](project-config.md) for
supported semantics and deliberate limits.
