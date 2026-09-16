v {xschem version=3.4.8 file_version=1.3}
G {}
K {}
V {}
S {}
F {}
E {}
T {VACASK native testbench: VOUT = 2 V (select Simulator > VACASK)} -30 -40 0 0 .3 .3 {}
N 0 40 140 40 {lab=VIN}
N 140 100 140 140 {lab=VOUT}
N 0 100 0 200 {}
N 0 200 140 200 {}
C {resistor.sym} 140 70 0 0 {name=R1 value=10k}
[
K {type=resistor format="@name @pinlist @value" template="name=R1 value=1k"}
L 4 0 -30 0 -20 {}
B 4 -7 -20 7 20 {fill=false}
L 4 0 20 0 30 {}
B 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout pinnumber=1}
B 5 -2.5 27.5 2.5 32.5 {name=N dir=inout pinnumber=2}
T {@name} 15 -18 0 0 .2 .2 {}
T {@value} 15 0 0 0 .2 .2 {}
]
C {resistor.sym} 140 170 0 0 {name=R2 value=20k}
C {vsource.sym} 0 70 0 0 {name=V1 value=3 vacask_params="dc=3 mag=1"}
[
K {type=vsource format="@name @pinlist @value@savecurrent" template="name=V1 value=3 savecurrent=false"}
A 4 0 0 20 0 360 {}
L 4 0 -30 0 -20 {}
L 4 0 20 0 30 {}
L 4 -5 -10 5 -10 {}
L 4 0 -15 0 -5 {}
L 4 -5 10 5 10 {}
B 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout pinnumber=1}
B 5 -2.5 27.5 2.5 32.5 {name=M dir=inout pinnumber=2}
T {@name} -60 -18 0 0 .2 .2 {}
T {@value} -60 0 0 0 .2 .2 {}
]
C {gnd.sym} 0 200 0 0 {name=l1 lab=0}
[
K {type=label template="name=l1 lab=0"}
L 4 0 0 0 10 {}
L 4 -10 10 10 10 {}
L 4 -6 15 6 15 {}
L 4 -2 20 2 20 {}
B 5 -2.5 -2.5 2.5 2.5 {name=p dir=inout}
]
C {code.sym} 280 40 0 0 {name=SIM value="control
  abort always
  analysis op1 op
  analysis tran1 tran step=1n stop=10n
  analysis ac1 ac from=1 to=1G mode=\\"dec\\" points=10
endc"}
[
K {type=netlist_commands format=@value template="name=SIM value=.op"}
T {@value} 0 0 0 0 .2 .2 {}
]
T {VIN} 60 15 0 0 .2 .2 {}
T {VOUT} 155 105 0 0 .2 .2 {}
