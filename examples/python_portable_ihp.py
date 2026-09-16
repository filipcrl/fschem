"""Explicit migration recipe for native ihp-adc schematics (legacy/schem).

Already-portable active sheets use ihp-adc/experiments.py, without exclusions.
No inputs are modified. Pass absolute external paths; use --prepare-only for ADC.
See docs/portable.md for the explicit native-code migration choices.
"""
import argparse
from pathlib import Path
import fschem


def experiment(project, circuit):
    root = project.root
    if circuit == "inverter":
        return fschem.Simulation(
            project.load("inv_tb.sch"), mode="portable",
            parameters={"wp": "3u", "wn": "1.5u", "l": "0.2u"},
            exclude=["/MODELS", "/CONTROL", "/xinv/PARAMS"],
            corners="tt", temperature_c=27,
        ).op().tran(stop=20e-9, output_step=20e-12)
    if circuit != "adc":
        raise ValueError("circuit must be inverter or adc")
    parameters = dict(VIN_OFFSET="0.2", INV_WP="4.5u", INV_WN="1.5u", INV_L="0.2u",
                      FCLK="80Meg", NCYCLES="100", TSTART="1u", TCLK="1/FCLK",
                      TEDGE="0.005*TCLK", TSTOP="TSTART+NCYCLES*TCLK")
    capacitors = dict(S1="0.125", S2="0.06631097561", CUNIT="10f",
                      CS1_BASE="CUNIT", CI1_BASE="CUNIT", CS2_BASE="CUNIT", CI2_BASE="CUNIT",
                      CFFIN_BASE="CUNIT", CFFY1_BASE="2*CUNIT", CFFY2_BASE="CUNIT")
    return fschem.Simulation(
        project.load("adc_tb.sch"), mode="portable", parameters=parameters,
        # inv/tg PARAMS duplicate interface defaults: exclude, do not recreate as locals.
        exclude=["/MODELS", "/CONTROL", str(root / "inv.sch") + "#PARAMS",
                 str(root / "tg.sch") + "#PARAMS", str(root / "adc.sch") + "#CAP_SCALING"],
        local_parameters={"/xadc": capacitors},
        # MODELS also contained a real device, not just model configuration!
        sources={"/VCLKEN": {"kind": "voltage", "nodes": ["/CLK_EN", "/0"],
                               "value": "PWL(0 1 {TSTOP} 1 {TSTOP+TEDGE} 0)"}},
        corners="tt", temperature_c=27,
    ).op().tran(stop=2.25625e-6, output_step=125e-12)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pdk", type=Path, required=True, help="IHP-Open-PDK root")
    parser.add_argument("--schematics", type=Path, required=True, help="ihp-adc/legacy/schem (native snapshot)")
    parser.add_argument("--devices", type=Path, required=True, help="Xschem devices symbol library")
    parser.add_argument("--backend", choices=["ngspice", "vacask"], required=True)
    parser.add_argument("--circuit", choices=["inverter", "adc"], default="inverter")
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--home", type=Path, help="explicit simulator HOME (optional clean startup)")
    args = parser.parse_args()
    project = fschem.Project(args.schematics, discover_libraries=False, config=False,
        library_paths=[args.pdk.resolve() / "ihp-sg13g2/libs.tech/xschem", args.devices.resolve()],
        technology=fschem.IHP(args.pdk))
    prepared = experiment(project, args.circuit).prepare(backend=args.backend)
    run = prepared.write(args.directory)
    if not args.prepare_only:
        run.execute(timeout=120, env={"HOME": str(args.home.resolve())} if args.home else None)
    print(run.status, run.directory)


if __name__ == "__main__":
    main()
