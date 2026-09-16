# Read-only text tabs

Ordinary Open, drag/drop, and a positional CLI file open `.py`, `.spice`, `.sim`, `.log`, `.lib`, `.va`, `.json`, `.toml`, and `.txt` as text (case-insensitive). `.sch`/`.sym` still open schematics and `.raw` still opens the detached waveform window.

Use **Open as text…** in File or the toolbar, or `fschem --open-text FILE`, to view any file explicitly as text, including schematic source. The existing Original file text popup previews the loaded schematic (including unsaved edits); its **Open disk source in text tab** button instead views the saved disk file. Text tabs never execute code or write files. They cannot invoke schematic save, export, editing, undo/redo or hierarchy actions. Switching away preserves schematic hierarchy and undo/dirty state; closing a text tab needs no save prompt.

Controls: selectable monospace lines, line numbers, **Copy loaded text**, case-sensitive literal **Next match / Previous match** (one match per line, cyclic), **Go to line**, **Wrap**, **Reload**, and **Follow**. Follow defaults on for `.log`, checks once per second in the background, and moves to the final page after changes. Disable Follow to browse without following growth. A failed reload retains the last readable content and displays the error. Replacement, truncation and disappearance are handled by rereading the path.

## Deliberate bounds

- Reads run off the UI thread, with at most one pending read per text document. Closing a tab drops its receiver; completions cannot land in another tab.
- Only the first **4 MiB** are loaded. An explicit banner reports truncation. Follow is not a tail reader beyond that bound.
- Text displays in **100-line pages**; next/previous page, search and go-to-line navigate these. Each displayed line is capped at **4096 Unicode characters** with an explicit marker, bounding pathological layout work. Copy loaded text retains the full loaded prefix, including undisplayed line suffixes.
- Invalid UTF-8 is displayed with replacement characters. Copy is decoded text, not a byte-exact export. Search covers the loaded prefix, including long-line suffixes that are not displayed.
- No syntax highlighting, editing, regex, cross-line search, run-manifest integration or script launch. Follow only polls the active tab (reactivating catches up). No OS filesystem watcher or worker cancellation for stalled filesystem calls.

## Validation

`cargo test --all-targets` includes asynchronous reload/change/error, bounded-read, extension dispatch and schematic/text tab isolation tests, alongside existing hierarchy/edit/undo and waveform regression tests. Graphical smoke: `cargo run -- --smoke-test Cargo.toml`, `cargo run -- --smoke-test`, and `cargo run -- --open-text --smoke-test examples/demo.sch`. These render and close; manual interaction review is still useful for platform-specific selection/menu behavior.
