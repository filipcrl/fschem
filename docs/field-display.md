# Schematic field display

Expanded symbol fields (for example `@value`) display at most **48 Unicode
characters**, including a trailing `...`. This bounds long PULSE/PWL expressions
on the canvas. Free-standing annotation text is not shortened.

Set `display_max_chars=32` on an instance to override the limit; `0` disables
elision for that instance. This is a presentation property, accepted by portable
netlisting but never used to change a circuit value. Select the instance and use
the property inspector/editor to see or edit the complete value. Saving preserves
the full source; field text bounds and hit testing use the displayed value.

The toolbar's simulator dropdown immediately to the left of ▶ selects the same
ngspice/VACASK state as the Simulator menu. This remains the legacy native run
button; portable Python experiments are launched by their Python scripts.
