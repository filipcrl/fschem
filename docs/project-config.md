# Automatic project configuration

Put `.fschem.toml` in a project directory. At startup, fschem searches from the
input schematic's directory (or the current directory if no input was supplied)
upward and loads the **nearest** file. Ancestor configurations are not merged.
This works for the GUI, IR inspection and native netlisting/simulation. Opening
another file inside an already running GUI retains that session's library paths.

Example equivalent to setting `PDK_ROOT`, `PDK` and passing `-L`:

```toml
schema_version = 1
pdk_root = "~/work/IHP-Open-PDK"
pdk = "ihp-sg13g2"
library_paths = [
  "$PDK_ROOT/$PDK/libs.tech/xschem",
  "schem",
  "../xschem/xschem_library",
]
simulator = "vacask" # optional; otherwise ngspice
```

* Paths are relative to the **config directory**, not the shell's current directory.
* `~/`, `$NAME`, `${NAME}` and `$env(NAME)` expand without evaluating Tcl or shell
  code. Unknown variables are errors. `pdk_root`/`pdk` supply scoped `PDK_ROOT`/`PDK`
  values to library paths and schematic symbol references; other variables come
  from the environment. The process/shell environment is not modified.
* Resolution order remains document-local symbols, explicit CLI `-L` paths,
  configured library paths, then environment/system library discovery. Explicit
  `--simulator` overrides the configured default. Native `--netlist-format` is
  independent and still defaults to SPICE.
* `--config FILE` selects an explicit config; `--no-config` disables discovery.
  They are mutually exclusive. Malformed TOML, unknown keys, unsupported schema
  versions, invalid simulators and missing variables are errors, not fallbacks.
* Configuration is declarative and bounded to a regular file of at most 64 KiB.
  Opening it never executes scripts or runs simulations.

Python uses the same Rust loader:

```python
import fschem
project = fschem.Project(".")
print(project.config_path, project.library_paths)
circuit = project.load("schem/adc_tb.sch")
```

`Project(..., config=False)` disables config; `config="custom.toml"` chooses a
file (relative to the current directory). Explicit `library_paths` come first,
and explicit `technology` overrides config-derived `IHP` technology. Automatic
IHP selection requires `pdk="ihp-sg13g2"` and `pdk_root`; no PDK is installed,
copied or modified. `discover_libraries=False` disables legacy environment/system
**library discovery**, not project configuration (use `config=False` too for a
fully explicit project). Python backend selection remains explicit in
`sim.prepare(backend=...)`; `simulator` is a CLI/GUI default.

For `ihp-adc`, run the installed `fschem schem/adc_tb.sch` from its root with no
library flags. From the fschem source repository, `cargo run --
../ihp-adc/schem/adc_tb.sch` discovers the same project config.
