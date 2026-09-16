"""Opt-in read-only external integration; outputs always under pytest tmp_path."""
import os
from pathlib import Path
import runpy
import shutil
import numpy as np
import pytest
import fschem

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture
def project():
    names = ['FSCHEM_IHP_ROOT', 'FSCHEM_ADC_ROOT', 'FSCHEM_XSCHEM_DEVICES']
    if not all(os.environ.get(n) for n in names):
        pytest.skip('set explicit IHP/ADC/Xschem integration paths')
    pdk, adc, devices = [Path(os.environ[n]).resolve() for n in names]
    # This fixture exercises the explicit native-to-portable migration recipe,
    # not the already migrated active ihp-adc experiment factory.
    source = adc / 'legacy/schem' if (adc / 'legacy/schem').is_dir() else adc / 'schem'
    return fschem.Project(source, discover_libraries=False, config=False,
        library_paths=[pdk / 'ihp-sg13g2/libs.tech/xschem', devices], technology=fschem.IHP(pdk))


@pytest.mark.parametrize('backend', ['ngspice', 'vacask'])
def test_real_ihp_inverter(project, tmp_path, backend):
    if not shutil.which(backend):
        pytest.skip('engine unavailable')
    make = runpy.run_path(str(ROOT / 'examples/python_portable_ihp.py'))['experiment']
    sim = make(project, 'inverter')
    source = sim.circuit.path.read_bytes()
    (tmp_path / 'home').mkdir()
    run = sim.run(backend=backend, directory=tmp_path / 'run', timeout=60,
                  env={'HOME': str(tmp_path / 'home')})
    assert run.result('op').voltage('/OUT')[0] > 1.15
    tran = run.result('tran')
    selected = (tran.time > 2e-9) & (tran.time < 4e-9)
    assert np.all(tran.voltage('/OUT')[selected] < .1)
    assert np.all(tran.voltage('/IN')[selected] > 1.1)
    assert sim.circuit.path.read_bytes() == source
    assert len(fschem.Run.open(run.directory).results) == 2


@pytest.mark.parametrize('backend', ['ngspice', 'vacask'])
def test_adc_preparation(project, tmp_path, backend):
    make = runpy.run_path(str(ROOT / 'examples/python_portable_ihp.py'))['experiment']
    sim = make(project, 'adc')
    prepared = sim.prepare(backend=backend)
    assert 'sg13_lv_nmos' in prepared.deck and 'sg13_lv_pmos' in prepared.deck
    assert 'v(CLK_EN)' in prepared.deck
    assert 'CLK_EN 0' in prepared.deck
    run = prepared.write(tmp_path / 'adc')
    assert run.status == 'prepared'
    assert run.manifest['portable']['sources']['/VCLKEN']['kind'] == 'voltage'
