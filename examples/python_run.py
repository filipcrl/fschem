"""Run a native divider in a fresh directory; requires the selected engine on PATH.

Example: python examples/python_run.py --backend vacask --directory /tmp/divider-run
Native startup/environment configuration is inherited unless explicitly overridden.
"""
import argparse
from pathlib import Path

import fschem


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["ngspice", "vacask"], required=True)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    project = fschem.Project(Path(__file__).resolve().parents[1], discover_libraries=False)
    example = "spice" if args.backend == "ngspice" else "vacask"
    circuit = project.load(f"examples/{example}.sch")
    sim = fschem.Simulation(circuit, mode="native")
    try:
        run = sim.run(backend=args.backend, directory=args.directory, timeout=30)
    except fschem.SimulationError as error:
        print(f"{error.run.status}: {error}; artifacts: {error.run.directory}")
        raise SystemExit(1) from error
    run = fschem.Run.open(run.directory)
    for identifier, result in run.results.items():
        print(identifier, result.name, tuple(result.signals))
    node = "vout" if args.backend == "ngspice" else "VOUT"
    print("Divider voltage:", run.result("Operating Point").voltage(node))


if __name__ == "__main__":
    main()
