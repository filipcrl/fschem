# Linked schematic waveform graphs

Select a schematic graph, or hover over it when none is selected, and press **E**.
The waveform window opens its raw file, dataset, selected traces, scales and layout.
Away from graphs, E retains normal schematic hierarchy descent.

Changing the selected waves, dataset, projection, log axis or grouping in the
window updates its originating schematic graph through normal edit history.
Schematic property edits and undo/redo update the linked window in return.
Changes mark the schematic dirty; **save explicitly** to persist them. Switching
tabs does not redirect the link to another document.

Choose **Separate** in the waveform viewer to stack one trace per panel, **By
unit** to group like quantities, or **Overlay** to share a panel. The corresponding
schematic property is `groupmode=separate`, `unit`, or `overlay`. Embedded panels use
the same units, projections and waveform rendering as the detached window.

Hover within 18 screen pixels of a trace in an embedded graph for the nearest-trace
marker and x/y readout with units. This also works in stacked panels. Hover does not
consume selection/drag events, and markers are suppressed while a pointer button
is held.

Calculated traces added in the window are stored as bounded declarative expressions
in `fschem_expressions` when linked. Their exact selected names (including spaces)
round-trip through `node`. Reopening or undo recalculates them from the raw data;
the raw file is never modified. See [units and expressions](waveform-units.md).

Links have session-only document/record identities. Closing/reloading/deleting the
owner or making an ambiguous structural replacement **detaches** the window rather
than risking edits to a different graph; its waveform data remains available and
the status indicates detachment. Press E again to reconnect. Waveform edits attempted
while a schematic move blocks edits are reverted to the source configuration, not
queued for later. Invalid/missing datasets are shown as unavailable, not substituted
with selected traces from another dataset.
