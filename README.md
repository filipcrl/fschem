# fschem

A cross-platform **Rust + egui** schematic editor foundation, inspired by
Xschem. Open and edit **Xschem `.sch` and `.sym` files**, with a lossless document
model, command-based undo/redo, SPICE export, and waveform viewing.

## Run

Use Rust **1.90 or newer**. From this directory:

```sh
cargo run
cargo run -- examples/demo.sch
cargo run -- examples/resistor.sym

# With the neighboring Xschem checkout:
cargo run -- ../xschem/xschem_library/ngspice/opamp_65nm.sch

# Explicit project / PDK symbol libraries (repeat -L as needed):
cargo run -- -L /path/to/xschem_library -L /path/to/pdk/symbols /path/to/design.sch
```

The built-in demo embeds all of its symbols, so it works without an Xschem
installation. Choose it from **File → Built-in demonstration**.

macOS uses the normal Rust/Xcode Command Line Tools setup. Windows uses the
Rust MSVC toolchain and Visual Studio C++ build tools. Linux may need desktop
development packages such as:

```sh
sudo apt-get install build-essential pkg-config libxkbcommon-dev libwayland-dev libgl1-mesa-dev libx11-dev
```

The viewer uses eframe's OpenGL backend and the operating system's normal
windowing system. Linux file dialogs use `rfd`'s desktop portal backend; a
working desktop portal service is needed for those dialogs. Opening via CLI
does not need a file dialog. There is no Xschem C, Tcl/Tk, Cairo, or XQuartz
runtime dependency. CI is configured for macOS, Windows, and Linux; local GUI
verification so far has been on macOS.

## Current features

- Open `.sch` / `.sym` through the file panel, command line, or drag-and-drop.
- Background document loading; a failed load keeps the previous document open.
- Render wires, symbols, lines, rectangles, ellipses/arcs, polygons, quadratic
  Béziers, labels, bus widths, dashes, fills, and orthogonal-wire junction dots.
- Quarter-turn rotations, mirroring, composed symbols, and embedded definitions.
- Symbol resolution with ordered library paths and a per-load document cache.
- Basic attribute expansion: instance properties, symbol template defaults,
  `@name`, `@value`, `@symname`, and indexed pin attributes such as
  `@#0:pinnumber`.
- Pan/zoom, fit, adjustable display grid, layer visibility and hidden-text view.
- Click/Shift-click and box selection for components, wires, symbol graphics,
  text, and pins; selected items render above other drawing content.
- Move, copy, rotate, mirror, delete, and draw connected wires; save `.sch`/`.sym`
  files with atomic replacement and per-document command-based undo/redo.
- Click an instance to inspect its original properties and resolved symbol path.
- Select an instance and press **i** to view its symbol, including embedded
  definitions. **e** opens its schematic; **Shift-E** returns one level up.
  Double-click and the inspector's Symbol/Schematic buttons use the same actions.
- Files open in separate tabs, each with its own symbol/schematic navigation
  stack. Tab labels show the hierarchy (for example `adc_tb>adc`) and `*` marks
  edited frames. Each successful transition pushes the parent view; Shift-E pops
  exactly one frame and restores its viewport, layer visibility and selection.
  There is no eight-entry history cutoff. Failed descents do not push a frame,
  and going up cancels an in-flight child load. The toolbar shows the stack depth.
- Original-source preview and compatibility diagnostics. Missing/cyclic/generated
  symbols are indicated in the drawing instead of silently disappearing.
- Export SPICE decks from the current schematic, including subcircuit hierarchy.
- Choose ngspice or VACASK from the Simulator menu and run from the ▶ toolbar button.
- Plot ngspice raw data in a native waveform window or schematic graph rectangles,
  using a shared peak-preserving renderer with automatic file refresh.

### Command-line netlisting and simulation

Both modes run without opening a window. With an installed `fschem` binary:

```sh
# Generate SPICE (default) or native VACASK netlists:
fschem --netlist divider.spice examples/spice.sch
fschem --netlist-format spice --netlist divider.spice examples/spice.sch
fschem --netlist-format vacask --netlist divider.sim examples/vacask.sch

# Generate a deck and run the selected simulator:
fschem --simulator ngspice --simulate divider.raw examples/spice.sch
fschem --simulator vacask --simulate divider.raw examples/vacask.sch
```

For a source checkout, replace `fschem` with `cargo run --`. Add repeatable
`-L /path/to/library` options as needed. Output parent directories must exist.

`--netlist-format` accepts `spice` (default; `ngspice` is an alias) or `vacask`.
`--simulator` accepts `ngspice` (default) or `vacask`, and applies only to
`--simulate`. Simulation always generates the matching native deck; it saves
`.spice` or `.sim` and `.log` companions beside the required `.raw` destination.
The selected simulator must be on `PATH`. VACASK schematics need native testbench
code; choosing VACASK does not translate ngspice commands.

Use exactly one input schematic and one mode: `--netlist` or `--simulate`.
Neither combines with `--check`, `--smoke-test`, or `--screenshot`. Successful runs
print output paths; errors go to stderr with a nonzero exit status. Existing raw
results are retained if simulation fails. See `fschem --help` for all options.

### SPICE netlist export

Choose **File → Export SPICE netlist…** and save a `.spice`, `.sp`, or `.cir`
file. Export runs in the background and reports completion or errors in the UI.
The command is available for schematics, including the built-in demonstration.
For command-line export without opening a window:

```sh
cargo run -- --netlist target/divider.spice examples/spice.sch
cargo run -- -L /path/to/xschem_library --netlist design.spice design.sch
# Optional, with ngspice installed (the example's VOUT should be 2 V):
ngspice -b target/divider.spice
```

The exporter follows Xschem's static SPICE conventions:

- Symbol `format` strings and `template` defaults, with instance overrides;
  `@name`, `@value`, `@model`, `@symname`, `@spiceprefix`, `@pinlist`, `@@PIN`,
  and indexed pin attributes such as `@#0:net_name` and `@#0:pinnumber`.
- Pins are layer-5 rectangle centers, in symbol record order (not `pinnumber`
  order), with instance rotation and mirroring applied.
- Wire endpoints, T-junctions, overlaps, and pins on wire interiors connect,
  including diagonal segments. Crossed wire interiors alone do not connect.
- Wire `lab` attributes and label/port symbols name nets; matching labels join
  disconnected segments. Ground symbols with `lab=0` produce SPICE node `0`.
  A literal `GND` label remains `GND`. Unnamed nets receive stable `net1`,
  `net2`, … names, including floating pins.
- Schematic `S` records and `netlist_commands` symbols supply SPICE directives;
  `device_model` text is deduplicated. `spice_ignore`, `only_toplevel`, and
  top-level `@savecurrent` are supported.
- `type=subcircuit` symbols produce calls and recursively resolved `.subckt`
  definitions. Port order comes from the symbol; schematic port labels must
  match. Resolution honors `schematic=` using the navigation/library rules.
  `spice_stop=true` leaves a call to an externally supplied subcircuit.

This is a **static, scalar netlister**, not full Xschem/Tcl compatibility.
Bus/vector names, Tcl-generated formats/values, `spice_short`, and hierarchical
current-save expansion are not supported. Missing symbols/format attributes,
conflicting net labels, duplicate device names, and hierarchy errors fail the
export before writing a deck. Supply model includes and simulation commands in
the schematic; the export-only command does not invoke a simulator. Source schematics and symbols
are not modified. `examples/spice.sch` is a self-contained runnable divider.

### Run a simulation and save raw data

Choose **ngspice** (default) or **VACASK** in the **Simulator** menu, then click
**▶** in the toolbar. The selection applies to subsequent runs in this session.
Choose a `.raw` destination; fschem generates the netlist and runs the selected
simulator in the background. With ngspice and a destination named `divider.raw`,
the output files are:

- `divider.raw` — ngspice raw simulation data.
- `divider.spice` — generated SPICE deck.
- `divider.log` — ngspice output and error messages.

Install `ngspice` and make it available on the application's `PATH` (for example,
`brew install ngspice` on macOS). The schematic must supply its analyses through
SPICE directives such as `.op`, `.tran`, `.ac`, or `.dc`, or corresponding commands
inside a `.control` block. For control-block analyses, the temporary run deck
adds a raw-data write before quitting; it saves the current plot. Relative model
and include paths are resolved from the schematic's directory.

The status bar reports completion and output paths. Errors include the log path
and the triggering diagnostics, even when they occur before the recent log tail.
Startup OSDI preload failures are shown as non-blocking notices if ngspice exits
successfully and produces raw data; circuit/model and analysis failures still
fail the run. A failed simulation does not replace an existing raw
file. Try `examples/spice.sch` for an operating-point simulation with VOUT = 2 V.

#### VACASK

Install `vacask` on the application's `PATH`. No `ng2vc` or Python converter is
used: fschem generates native VACASK syntax directly from schematic connectivity.
Try **`examples/vacask.sch`**, select **Simulator → VACASK**, and click **▶**.
The example runs operating point, transient, and AC analyses (VOUT = 2 V at DC).

Write native testbench syntax in a `netlist_commands` symbol's `value`:

```text
control
  abort always
  analysis op1 op
  analysis tran1 tran step=1n stop=10n
  analysis ac1 ac from=1 to=1G mode="dec" points=10
endc
```

Commands are passed through, not translated, so other VACASK analyses and control
constructs are available too. Use `abort always` to stop on failed analyses;
VACASK otherwise allows failed analyses to continue and can return partial results.
For a schematic shared with ngspice, set `vacask_code` on the code instance to keep
native commands separate from its SPICE `value`. The schematic's **E** global block
provides the native preamble (`include`, `parameters`, models); **S** remains SPICE.

Built-in native mappings cover two-terminal resistors, capacitors, inductors,
and voltage/current sources. Their `value` is a **VACASK** value (suffixes are
case-sensitive). Set `vacask_params` for native parameter assignments such as
`dc=3 mag=1` or `type="pulse" val0=0 val1=1 rise=1n fall=1n width=5n period=10n`.
SPICE `PULSE(...)` values and dot-card analyses are not converted.

Other devices and subcircuit symbols need `vacask_format`, for example
`@name (@pinlist) @model` or `@name (@pinlist) @symname`. It uses the same pin and
property substitutions as SPICE formats. Put native model/load declarations in
`vacask_model` (deduplicated), a code block, or the E preamble. Native hierarchy
uses `subckt`/`ends`; `vacask_stop=true` leaves its definition to your includes.
Use `vacask_ignore=true` to omit an instance from native netlisting.

fschem runs `vacask -qp -sp` in an isolated output directory and searches the
schematic directory for relative includes and device loads. Native `.sim` and
`.log` files are saved beside the chosen `.raw` file. All top-level `.raw` files
created by the testbench are combined in filename order for the waveform viewer;
use plain analysis names, not paths. Previous results are retained on a failed
run or malformed output. Postprocessing scripts are skipped.

### Plot raw simulation data

Successful simulations automatically open their raw results in a **separate
Waveforms window**. Click **Open raw…** in the schematic toolbar to browse an
existing `.raw` file, or drag a raw file onto the schematic window. Ngspice does
not need to be installed to view an existing file.

In the waveform window:

- Check signals in the **Signals** list; filter by name or use **All / None**.
- Select an analysis from the plot dropdown when a file contains multiple plots.
- Drag to pan, use the scroll wheel or pinch to zoom only the X axis around the
  pointer, and double-click or press **Fit** to reset the
  view, and hover over a trace for numeric coordinates.
- Traces connect samples with straight lines. Cursor values interpolate linearly
  between original samples in the displayed axes, including when display LOD is
  active; gaps are never interpolated across.
- Choose **Real**, **Imaginary**, **Magnitude**, **Magnitude (dB)**, or
  **Phase (degrees)**. AC data defaults to magnitude versus logarithmic frequency;
  **Log X** toggles logarithmic X scaling. dB values are relative to one unit.
- Operating-point results use point markers and show numerical values in the
  signal list. Signal units appear in the legend; selected signals share a Y axis.
- Use **Reload** after another program updates the file. Closing this window
  leaves the schematic open.

Loading runs in the background. A failed load shows an error and preserves the
previous plot. The reader supports ngspice ASCII and native-endian binary-double
raw files, real and complex data, and concatenated plots. Binary files must use
the current machine's byte order; LTspice-specific layouts and unpadded variable-
length data are not supported. Files are limited to 256 MiB and 16 million total
samples. Non-finite samples and values ineligible for logarithmic display are
shown as gaps. On backends without native child-window support, the plot uses
an embedded egui window instead.

#### Graphs inside schematics

Select a graph to edit **Graph properties** in the Properties panel. Enter a
source path in `rawfile`, or click **Browse raw file…**. Clearing the path restores
automatic source selection. Graph property edits support undo/redo and are saved
with the schematic; selecting several graphs applies edits to all of them.

Rectangles with `flags=graph` display raw data automatically. `rawfile` names an
absolute path or a path relative to the schematic directory; without it,
`circuit.sch` loads `circuit.raw` alongside the schematic. Visible graphs check
for creation and changes every 500 ms. Parsing runs in the background, and the
last valid plot remains visible while an incomplete replacement is retried.
The separate waveform window also refreshes automatically, preserving its signal
selection and view.

Graph `node` entries select raw signals (either `out` or `v(out)`); `dataset`
selects a zero-based raw plot. Numeric `x1`, `x2`, `y1`, `y2` bounds and `logx`
are supported. Without explicit bounds, axes fit the selected data. Embedded
plots leave mouse gestures to the schematic editor. Tcl paths, signal arithmetic,
logarithmic Y axes and full Xschem sweep/dataset semantics are not implemented.

#### Waveform performance

The waveform window repaints independently of the schematic. Trace projection
and indexing run in the background, and toggling a previously prepared signal
reuses its cache. The signal list renders only its visible rows.

Rendering uses a viewport-aware extrema index: off-screen samples are skipped,
and subpixel groups retain their first/last samples and minimum/maximum values.
This preserves narrow spikes and gaps while keeping rendering work proportional
to the window's pixel width. Zooming in restores the original samples; raw data
is never permanently reduced. Fit bounds come from the complete traces, and
hover/Y-only changes reuse the prepared display geometry.

The reproducible synthetic benchmark (four million-sample traces, CPU frame
preparation and tessellation) measured approximately **2,511 ms → 11.9 ms per
frame** in a local development build, about **211× faster**. One-time indexing
of a million-sample trace took approximately 67 ms. Results vary by hardware:

```sh
cargo test --locked --bin fschem benchmark_large_waveform_frames -- --ignored --nocapture
```

### Controls

On macOS, File, Edit, View, and Navigate use the native system menu bar. View includes
Libraries… (Command-,), and Quit is in the standard application menu. View
checkmarks and navigation availability follow the current document and selection.
Other platforms use the in-window menu bar.

| Control | Action |
| --- | --- |
| Ctrl/Command-N | Create a blank schematic (.sch) or symbol (.sym) in a new tab; existing files are never overwritten |
| Ctrl/Command-O | Open a schematic or symbol in a new tab |
| Ctrl/Command-W | Close the active tab |
| Ctrl/Command-R | Reload the current file and its symbol dependencies |
| Ctrl/Command-S | Save; documents without a path open a save dialog |
| Ctrl/Command-Shift-S | Save As |
| Ctrl/Command-Z | Undo |
| Ctrl/Command-Shift-Z | Redo (also Ctrl-Y on Windows/Linux) |
| Left-click | Select an item; repeated clicks cycle overlaps; empty space clears selection |
| Shift-click / Shift-left-drag | Add items / enclosed items to the selection |
| Left-drag | Box-select enclosed items |
| G / C | Move / copy the selection; left-click places it |
| R / X / Y | Rotate 90° / mirror across X or Y while moving or copying |
| W | Start a wire; clicks add segments; clicking a port or existing wire finishes |
| Esc | Cancel an unplaced move/copy or unfinished wire segment |
| Backspace | Delete selected items |
| Middle-button or right-button drag | Pan; left dragging does not move the view |
| Mouse wheel / pinch | Zoom at pointer |
| Shift-scroll | Pan with scrolling |
| F / Home | Fit visible drawing layers |
| i | Descend into the selected instance's symbol |
| e | Descend into the selected instance's schematic |
| Shift-E | Pop one frame from the active tab's navigation stack |
| Double-click component | Descend into its symbol (same as i) |
| Up button | Same stack pop as Shift-E |

Moves and copies snap their displacement to the grid. Rotation/mirroring uses
the point grabbed by the cursor as its pivot. New wire routes use only horizontal
and vertical segments, inserting a right-angle bend when necessary. Press **R**
while wiring to toggle horizontal-first versus vertical-first routing. Every
committed edit rechecks wires, merging touching or overlapping collinear segments
with identical properties and splitting segments at branch connections. Moving
or deleting a branch removes obsolete splits; moving it onto another wire creates
a new split. Selections are remapped to the resulting segments. Unconnected interior crossings remain
unconnected, and labels are preserved when splitting. Escape
keeps already placed wire segments. The Edit menu displays available actions
and their shortcuts; rotation and mirroring require a move/copy in progress.

When editing a `.sym` file or an embedded symbol, the inspector shows a **Symbol
tools** panel. Create lines, rectangles, circles, text, and pins; choose drawing
layers, dimensions, text scale, and pin name/direction before placement. Click the
canvas to place, use R/X/Y to rotate or mirror, or Esc to cancel. Pin names must be
unique. Existing selection, move, copy, delete, save, and undo/redo tools work on
the new primitives. These creation controls are hidden in schematic documents.

Each committed placement (including its rotations and mirrors), deletion, or
wire-placement click is one undoable command. A wire click that inserts a bend
is still one command. Selection and document text are restored together. Undo
during a move/copy cancels the preview first; during wiring it discards the
preview and undoes the last committed edit. Cancelled/no-op edits preserve redo;
a new committed edit clears it. History belongs to each document/navigation
frame and survives Save/Save As. Undo/redo never writes files automatically.

Editing and navigation shortcuts are inactive while typing in a text field.
Schematic descent honors
an instance's `schematic=` property first, then the symbol's `schematic=`
property; otherwise it looks for a `.sch` beside the resolved `.sym`, followed
by the usual document/library search. Static attribute substitutions such as
`@symname` and environment references are supported. Missing or dynamic targets
leave the current view in place and show a diagnostic.

Symbol and schematic navigation and the Up button use the active tab's stack.
Ordinary file opens start a new tab with an empty stack; switching tabs preserves
views and undo histories. Reload refreshes the current view without
pushing a frame. For an embedded symbol it rebuilds the current embedded
snapshot and its dependencies; reload its containing document to read a new
embedded definition from disk.

Changes stay in memory until Save. The status bar distinguishes modified, saved,
and unnamed documents. Saving an embedded-symbol view uses Save As to create a
standalone symbol file. Going up from an edited child, reloading an edited view,
closing a tab, or quitting offers Save / Don't Save / Cancel. Closing checks all
frames in the tab; quitting checks every tab. Cancelling a save dialog or a failed
save prevents closing. Reload replaces the current document and its edit history.

## Symbol libraries

The referring document's directory is searched first, followed by configured
library folders. Bare names also search each library's `devices/` subdirectory.
The viewer does not recursively search an entire disk or choose a random symbol
with a matching basename.

Startup paths come from:

1. Repeated `-L` / `--library` arguments.
2. `XSCHEM_LIBRARY_PATH` (OS path-list separator: `:` on Unix, `;` on Windows).
3. `XSCHEM_SHAREDIR/xschem_library` and the user's `.xschem/xschem_library`.
4. Common installation prefixes and a neighboring `xschem/xschem_library`
   checkout relative to the working directory.

Use **Libraries… → Add library folder…**, then **Reload current document with
these paths** to resolve missing project symbols. UI changes are session-only;
use CLI arguments or environment variables for repeatable startup settings.

References support absolute/relative paths, `~/`, `$env(NAME)`, `${NAME}`, and
`$NAME` using process environment variables. Library folder arguments themselves
should be resolved directory paths. `xschemrc`, library alias arrays, Tcl
variables that are not environment variables, and PDK startup scripts are not
evaluated by this milestone.

## Stroke font

The Xschem stroke font is **embedded in the executable** from
`assets/xschem-font.sch` using Rust's `include_str!`. Its glyphs are decoded once
and rendered as line segments. No font installation, filesystem lookup, path
setting or `--font` argument is needed. The Libraries window configures symbol
libraries only.

Native OS font integration is outside the current scope. Custom Xschem font
styling is not applied, and non-ASCII characters use the stroke font's `?` glyph.

### Licensing

The original Rust sources and demonstration files retain their MIT license
(`LICENSE`). The bundled Xschem font is GPL-2.0-or-later, attributed in
`assets/NOTICE.md`, with its license in `licenses/Xschem-GPL-2.0.txt`. Distributions
of the application containing this font must comply with those GPL terms; the
Cargo package metadata lists both component licenses.

## Compatibility contract

**File preservation and behavioral compatibility are separate goals.**

The reader handles Xschem file versions 1.0–1.3, including legacy unversioned
files and extensionless v1.0 symbol references. It understands the standard
`v`, `G`, `K`, `V`, `S`, `F`, `E`, `L`, `B`, `N`, `A`, `P`, `T`, `C`, and embedded
`[` / `]` records. Outer-field backslash escaping follows Xschem's file reader,
not Tcl's nested-brace/list grammar.

`Document::original()` retains the original UTF-8 source **byte-for-byte**:
line endings, indentation, comments, property spelling/order, duplicate keys,
unknown records, and trailing data. Typed records have byte spans into that
source, and properties keep their raw decoded text alongside lookup entries.
Unsupported records and malformed known records are preserved and diagnosed.
Unterminated strings/embedded blocks produce a load error. Invalid UTF-8 is
rejected rather than replaced with lossy characters.

Document edit methods return a new parsed document after replacing numeric spans,
inserting records, or removing records. Untouched source text remains intact;
copies receive unique instance/pin names. Shared embedded definitions are retained
when deleting an instance that introduced them. `original()` returns that document's
source text; directly mutating the typed records does not serialize those changes.

### Current interpretation limits

- Tcl expressions, generators and dynamic PDK behavior are retained, but not
  executed. Generated symbols get placeholders; direct `tcleval(...)` labels
  get an expression indicator. Other unresolved substitutions are diagnosed.
- Basic label substitution is a single pass; nested substitutions, full net
  names, simulation annotation, parameter inheritance and complete Xschem
  command semantics remain to be implemented.
- Embedded images are retained in the document but shown as rectangle boundaries.
  Schematic waveform graphs support the properties described above, not the full
  Xschem graph expression language.
- Stipple/custom fill patterns are approximated; curves are tessellated and
  rendering is not pixel-identical to Xschem. Rotated/mirrored text is kept
  readable using Xschem's stroke-font layout convention.
- Display junction dots are geometric cues; SPICE export uses its own electrical
  connectivity engine. Display dots on diagonal wire interiors are not computed yet.
- Symbol and schematic descent do not yet evaluate instance-specific
  circuit parameters or Tcl-generated hierarchy views.
- Practical loading bounds: 128 MiB per file, one million parsed records or
  scene drawing items, one million points per polygon, 64 embedded parsing
  levels and 32 levels of symbol expansion. Extreme/non-finite numeric fields
  are diagnosed and retained rather than sent to the GPU.

## Architecture

```text
src/document.rs     Lossless record reader, spans, properties, diagnostics
src/edit.rs         Reversible edit commands, immutable edit states, undo/redo history
src/geometry.rs     f64 coordinates, transforms, bounds and viewport math
src/library.rs      File loading, ordered resolution, environment expansion, cache
src/navigation.rs   Symbol/schematic targets and hierarchy filename resolution
src/netlist.rs      Electrical connectivity, static SPICE formats and hierarchy export
src/simulation.rs   Background-compatible ngspice runner, raw data and log output
src/raw.rs          Ngspice ASCII/binary raw-file reader, real and complex samples
src/scene.rs        Resolve documents into display geometry and inspection data
src/stroke_font.rs  Compiled-in stroke font, decoded and cached once

src/canvas.rs       egui painting, clipping, mesh preparation and display styles
src/viewer.rs       egui application, panels, background loading and navigation
src/waveforms.rs    Native waveform subwindow and automatic raw-file refresh
src/graph.rs        Shared waveform model, signal selection, axes and LOD rendering
src/schematic_graphs.rs Schematic graph bindings and background raw-file refresh
src/waveform_lod.rs Peak-preserving viewport queries and multiresolution extrema index
src/main.rs         CLI and native application entry point
```

The library modules have no egui types or dependencies. Desktop dependencies
are behind the default `gui` Cargo feature; the document/scene core also builds
and tests independently:

```sh
cargo test --no-default-features --lib
```

Document coordinates stay in `f64`; conversion to egui's `f32` screen coordinates
happens at the painting boundary. Static fill triangulation happens during
background loading, and strokes are clipped before dashing/GPU conversion.

## Verification

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo run -- --check examples

# Syntax/preservation checks over an existing installation or source library:
cargo run -- --check ../xschem/xschem_library

# Optional full scene-resolution check against external fixtures:
XSCHEM_TEST_LIBRARY=../xschem/xschem_library cargo test --test compatibility installed_library_scenes -- --ignored --nocapture

# Actual desktop window, render and exit; optional screenshot artifact:
cargo run -- --smoke-test --screenshot target/demo.png
cargo run -- --smoke-test ../xschem/xschem_library/devices/res.sym

# Optional real-ngspice raw-file compatibility test (OP, transient, AC; ASCII/binary):
cargo test --no-default-features --lib raw::tests::reads_ngspice_operating_point_transient_and_ac_in_both_encodings -- --ignored
```

`--check` recursively reads `.sch`/`.sym` files, reports malformed known records
as failures, and verifies retained bytes against the inputs. Unknown future
records are reported without discarding them. Use `--verbose` for all parser
diagnostics. This check validates parsing/preservation, **not** full library
resolution, electrical semantics, or rendering parity.

The initial checkout validation covered **1,903 Xschem library files and 128,104
top-level records**, with zero parser diagnostics. The optional scene import
test also assembled all 1,903 files into scenes (161,521 drawing items), reporting
1,746 compatibility notices for unresolved/dynamic or otherwise unsupported
content. These notices are distinct from parser errors. Tests additionally cover
escaping, CRLF/BOM/comments, malformed-record recovery, unknown data, embedded
symbols, template substitution, library precedence, space-containing paths,
missing/cyclic references, transforms, viewport math, and egui tessellation
without an OS window.

## Next milestone

Extend property editing and connectivity-aware tools on top of the command
history, and continue using existing Xschem projects as compatibility fixtures.


## Project configuration and waveform units

fschem automatically loads the nearest `.fschem.toml` for project library paths,
scoped PDK variables and the default simulator. See [project configuration](docs/project-config.md).
Long expanded schematic fields are [elided for display only](docs/field-display.md).
Waveform results preserve native type metadata and show electrical units;
[calculated traces](docs/waveform-units.md) propagate units such as V/V, Ω and W.
Press **E** on a [schematic graph](docs/linked-graphs.md) for its linked waveform
window; selections/layout synchronize both ways, including stacked panels. Embedded
graphs also show nearest-trace hover markers with unit-aware readouts.

## Python experiments and artifact inspection

The installable headless Python package supports native ngspice/VACASK runs and a
strict portable op/tran subset, immutable prepared artifacts, NumPy results,
explicit parameter scopes and narrow IHP LV wrapper binding. See
[installation/API](docs/python.md), [portable migration and ADC workflow](docs/portable.md),
and [compiler IR inspection](docs/ir.md). The GUI remains separately installed:
`fschem --open-run DIRECTORY` inspects run artifacts without executing code;
`fschem --open-text FILE.py` opens read-only source. Python imports never start a GUI.
