"""Run the same parameterized divider with either backend in a fresh directory."""
import argparse
from pathlib import Path
import fschem

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--backend', choices=['ngspice', 'vacask'], required=True)
parser.add_argument('--directory', type=Path, required=True)
parser.add_argument('--home', type=Path, help='explicit simulator HOME for clean startup')
args = parser.parse_args()
project = fschem.Project(Path(__file__).resolve().parents[1], discover_libraries=False)
sim = fschem.Simulation(project.load('examples/portable.sch'), mode='portable',
                        parameters={'R': '10k', 'VDD': 3}).op().tran(stop=1e-8, output_step=1e-9)
# This returns a distinct experiment and never modifies the schematic or sim.
experiment = sim.with_parameters(VDD=6)
run = experiment.run(backend=args.backend, directory=args.directory, timeout=30,
                     env={'HOME': str(args.home.resolve())} if args.home else None)
print(run.status, 'divider voltage:', run.result('op').voltage('/VOUT'))
