"""Run with installed fschem: python examples/python_native.py [--raw FILE]."""
import argparse
from pathlib import Path

import fschem


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw", type=Path)
    args = parser.parse_args()
    project = fschem.Project(Path(__file__).resolve().parent, discover_libraries=False)
    for source, backend in [("spice.sch", "ngspice"), ("vacask.sch", "vacask")]:
        circuit = project.load(source)
        assert not circuit.check(backend=backend)
        print(circuit.netlist(backend=backend))
    if args.raw:
        for result in fschem.read_raw(args.raw):
            print(result.name, {name: values.shape for name, values in result.signals.items()})


if __name__ == "__main__":
    main()
