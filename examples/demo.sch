v {xschem version=3.4.8 file_version=1.3
fschem demonstration. All symbols are embedded; no external library is needed.}
G {}
K {}
V {}
S {}
F {}
E {}
# Comments, custom properties and formatting are retained by the reader.
T {fschem | Xschem-compatible viewer} 0 -130 0 0 0.45 0.45 {}
T {Embedded symbols, properties, geometry and transforms} 0 -85 0 0 0.22 0.22 {}
N 0 0 140 0 {lab=VIN}
N 140 0 140 40 {}
N 140 100 140 150 {}
N 140 150 140 190 {}
N 140 150 300 150 {lab=VOUT}
N 140 250 140 290 {lab=GND}
C {resistor.sym} 140 70 0 0 {name=R1 value=10k custom_property="retained verbatim"}
[
v {xschem file_version=1.3}
G {}
K {type=resistor template="name=R1 value=1k" format="@name @pinlist @value"}
L 4 0 -30 0 -20 {}
L 4 0 -20 -8 -15 {}
L 4 -8 -15 8 -5 {}
L 4 8 -5 -8 5 {}
L 4 -8 5 8 15 {}
L 4 8 15 0 20 {}
L 4 0 20 0 30 {}
B 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout pinnumber=1}
B 5 -2.5 27.5 2.5 32.5 {name=N dir=inout pinnumber=2}
T {@name} 15 -18 0 0 .2 .2 {}
T {@value} 15 0 0 0 .2 .2 {}
]
C {resistor.sym} 140 220 0 0 {name=R2 value=20k}
T {VIN} 0 -25 0 0 .25 .25 {}
T {VOUT} 260 125 0 0 .25 .25 {}
T {GND} 160 275 0 0 .2 .2 {}
L 4 120 290 160 290 {}
L 4 125 297 155 297 {}
L 4 132 304 148 304 {}
T {Rotated + mirrored symbol} 370 15 0 0 .22 .22 {}
C {resistor.sym} 440 80 1 1 {name=R3 value=4.7k}
T {Drawing primitives} 370 170 0 0 .22 .22 {}
B 4 370 205 430 255 {fill=false dash=4}
A 6 485 230 25 30 270 {}
P 8 5 535 255 565 205 595 255 565 240 535 255 {fill=full}
P 4 3 370 295 470 245 595 295 {bezier=true}
