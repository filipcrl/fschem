# fschem

A cross-platform **Rust + egui** schematic editor foundation, inspired by
Xschem. The first milestone is a **read-only viewer for Xschem `.sch` and `.sym`
files**, with a lossless document model beneath the UI.

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
- Click an instance to inspect its original properties and resolved symbol path.
- Select an instance and press **i** to view its symbol, including embedded
  definitions. **e** opens its schematic; **Shift-E** returns one level up.
  Double-click and the inspector's Symbol/Schematic buttons use the same actions.
- One application-level navigation stack is shared by symbol and schematic
  descent. Each successful transition pushes the parent view; Shift-E pops
  exactly one frame and restores its viewport, layer visibility and selection.
  There is no eight-entry history cutoff. Failed descents do not push a frame,
  and going up cancels an in-flight child load. The toolbar shows the stack depth.
- Original-source preview and compatibility diagnostics. Missing/cyclic/generated
  symbols are indicated in the drawing instead of silently disappearing.

### Controls

On macOS, File, View, and Navigate use the native system menu bar. View includes
Libraries… (Command-,), and Quit is in the standard application menu. View
checkmarks and navigation availability follow the current document and selection.
Other platforms use the in-window menu bar.

| Control | Action |
| --- | --- |
| Ctrl/Command-O | Open a schematic or symbol |
| Ctrl/Command-R | Reload the current file and its symbol dependencies |
| Left-click | Select an instance (empty canvas clears selection) |
| Middle-button or right-button drag | Pan; left dragging does not move the view |
| Mouse wheel / pinch | Zoom at pointer |
| Shift-scroll | Pan with scrolling |
| F / Home | Fit visible drawing layers |
| i | Descend into the selected instance's symbol |
| e | Descend into the selected instance's schematic |
| Shift-E | Pop one frame from the global navigation stack |
| Double-click component | Descend into its symbol (same as i) |
| Backspace / Up button | Same stack pop as Shift-E |

The canvas uses the default arrow pointer, including while panning. Navigation
shortcuts are inactive while typing in a text field. Schematic descent honors
an instance's `schematic=` property first, then the symbol's `schematic=`
property; otherwise it looks for a `.sch` beside the resolved `.sym`, followed
by the usual document/library search. Static attribute substitutions such as
`@symname` and environment references are supported. Missing or dynamic targets
leave the current view in place and show a diagnostic.

Symbol and schematic navigation, ordinary file opens, and the Up/Backspace
aliases all use this one stack. Reload refreshes the current view without
pushing a frame. For an embedded symbol it rebuilds the current embedded
snapshot and its dependencies; reload its containing document to read a new
embedded definition from disk.

This milestone deliberately has no editing or schematic-save commands. It
does not write to source schematic/symbol files.

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

This is the foundation for a future span-based writer. `original()` is the
original input, **not a serializer for edits to the parsed record view**. No
claim of edited-file round-trip compatibility is made yet.

### Current interpretation limits

- Tcl expressions, generators and dynamic PDK behavior are retained, but not
  executed. Generated symbols get placeholders; direct `tcleval(...)` labels
  get an expression indicator. Other unresolved substitutions are diagnosed.
- Basic label substitution is a single pass; nested substitutions, full net
  names, simulation annotation, parameter inheritance and complete Xschem
  command semantics remain to be implemented.
- Waveform plots and embedded images are retained in the document but currently
  shown as rectangle boundaries.
- Stipple/custom fill patterns are approximated; curves are tessellated and
  rendering is not pixel-identical to Xschem. Rotated/mirrored text is kept
  readable using Xschem's stroke-font layout convention.
- Junction dots are geometric cues, not an electrical connectivity engine.
  Interior junctions on diagonal wires are not computed yet.
- Editing, undo, netlisting and simulation are future milestones. Symbol and
  schematic descent are supported, but do not yet evaluate instance-specific
  circuit parameters or Tcl-generated hierarchy views.
- Practical loading bounds: 128 MiB per file, one million parsed records or
  scene drawing items, one million points per polygon, 64 embedded parsing
  levels and 32 levels of symbol expansion. Extreme/non-finite numeric fields
  are diagnosed and retained rather than sent to the GPU.

## Architecture

```text
src/document.rs     Lossless record reader, spans, properties, diagnostics
src/geometry.rs     f64 coordinates, transforms, bounds and viewport math
src/library.rs      File loading, ordered resolution, environment expansion, cache
src/navigation.rs   Symbol/schematic targets and hierarchy filename resolution
src/scene.rs        Resolve documents into display geometry and inspection data
src/stroke_font.rs  Compiled-in stroke font, decoded and cached once

src/canvas.rs       egui painting, clipping, mesh preparation and display styles
src/viewer.rs       egui application, panels, background loading and navigation
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

Introduce undoable document commands and a lossless writer before exposing
editing. Keep tool state and electrical connectivity in the Rust core, and
continue using existing Xschem projects as compatibility fixtures.
