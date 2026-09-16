from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import signal
import sys
import threading
import time

import numpy as np
import pytest

import fschem

ROOT = Path(__file__).resolve().parents[2]
pytestmark = pytest.mark.skipif(os.name != "posix", reason="POSIX native runner")
RAW = 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n0 v(a:b) voltage\n1 v(a.b) voltage\nValues:\n0 2 3\n'


def prepared(backend="ngspice"):
    example = "spice" if backend == "ngspice" else "vacask"
    circuit = fschem.Project(ROOT, discover_libraries=False).load(f"examples/{example}.sch")
    return fschem.Simulation(circuit, mode="native").prepare(backend=backend)


def fake(tmp_path, code):
    path = tmp_path / "fake engine"
    path.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nimport os, sys, time\n" + code)
    path.chmod(0o755)
    return path


def success(tmp_path):
    return fake(tmp_path, f"Path('simulation.raw').write_text({RAW!r})\n")


def test_native_stages_reopen_and_stale_handle(tmp_path):
    p = prepared()
    assert "R1 VIN VOUT 10k" in p.deck
    r = p.write(tmp_path / "run")
    assert r.status == "prepared"
    stale = fschem.Run.open(r.directory)
    cwd, environment = Path.cwd(), dict(os.environ)
    r.execute(executable=success(tmp_path), env={"TEST_SECRET": "not-for-manifest"})
    assert r.status == "succeeded"
    assert Path.cwd() == cwd and dict(os.environ) == environment
    reopened = fschem.Run.open(r.directory)
    result = reopened.result("Operating Point")
    np.testing.assert_equal(result.voltage("a:b"), [2])
    np.testing.assert_equal(result.voltage("a.b"), [3])
    assert reopened.manifest["results"][0]["signal_map"] == {"v(a:b)": "v(a:b)", "v(a.b)": "v(a.b)"}
    assert "not-for-manifest" not in (r.directory / "run.json").read_text()
    snapshot = (r.directory / "run.json").read_bytes()
    with pytest.raises(ValueError, match="already executed"):
        stale.execute(executable=success(tmp_path))
    with pytest.raises(FileExistsError):
        p.write(r.directory)
    assert (r.directory / "run.json").read_bytes() == snapshot
    (r.directory / "simulation.raw").write_text("corrupt")
    with pytest.raises(ValueError, match="changed"):
        fschem.Run.open(r.directory)


@pytest.mark.parametrize("code,match", [
    ("sys.exit(7)", "status 7"),
    ("pass", "missing or incomplete"),
    ("Path('bad.raw').write_text('corrupt')", "raw"),
    (f"Path('simulation.raw').write_text({RAW!r}); print('Error: analysis failed')", "analysis error"),
])
def test_failures_are_durable(tmp_path, code, match):
    with pytest.raises(fschem.SimulationError, match=match) as error:
        prepared().run(directory=tmp_path / "run", executable=fake(tmp_path, code))
    run = error.value.run
    assert run.status == "failed"
    assert fschem.Run.open(run.directory).status == "failed"
    assert run.manifest["results"] == []
    assert (run.directory / "circuit.spice").is_file()
    assert (run.directory / "simulator.log").is_file()


def test_start_failure_and_changed_inputs(tmp_path):
    with pytest.raises(fschem.SimulationError):
        prepared().run(directory=tmp_path / "missing", executable=tmp_path / "absent")
    r = prepared().write(tmp_path / "edited")
    (r.directory / "circuit.spice").write_text("modified")
    with pytest.raises(fschem.SimulationError, match="input changed"):
        r.execute(executable=success(tmp_path))


def test_overrides_rejected():
    circuit = fschem.Project(ROOT).load("examples/spice.sch")
    for kwargs in ({"parameters": {}}, {"corners": []}, {"temperature_c": 27}):
        with pytest.raises(ValueError, match="native mode"):
            fschem.Simulation(circuit, mode="native", **kwargs)
    with pytest.raises(ValueError, match="mode must"):
        fschem.Simulation(circuit, mode="implicit")


def test_concurrent_fresh_runs_and_destination_race(tmp_path):
    engine = success(tmp_path)
    p = prepared()
    with ThreadPoolExecutor(4) as pool:
        runs = list(pool.map(lambda n: p.run(directory=tmp_path / str(n), executable=engine), range(4)))
    assert all(r.status == "succeeded" for r in runs)
    with ThreadPoolExecutor(2) as pool:
        futures = [pool.submit(p.write, tmp_path / "same") for _ in range(2)]
    assert sum(f.exception() is None for f in futures) == 1


def wait_file(path):
    for _ in range(200):
        if path.exists():
            return
        time.sleep(.01)
    pytest.fail(f"process did not create {path}")


@pytest.mark.parametrize("action", ["timeout", "cancel", "interrupt", "leader_exit"])
def test_process_group_cleanup(tmp_path, action):
    # Child repeatedly writes a heartbeat, even after its parent exits.
    child = "from pathlib import Path; import time\nwhile True:\n Path('heartbeat').write_text(str(time.time()))\n time.sleep(.01)\n"
    engine = fake(tmp_path, "import subprocess\n"
                  f"p = subprocess.Popen([sys.executable, '-c', {child!r}])\n"
                  "Path('child.pid').write_text(str(p.pid))\n"
                  "while not Path('heartbeat').exists(): time.sleep(.01)\n" +
                  (f"Path('simulation.raw').write_text({RAW!r})\ntime.sleep(.1)\n" if action == "leader_exit" else "time.sleep(30)\n"))
    directory = tmp_path / "run"
    cancel = threading.Event()
    def trigger():
        wait_file(directory / "heartbeat")
        if action == "cancel":
            cancel.set()
        elif action == "interrupt":
            os.kill(os.getpid(), signal.SIGINT)
    thread = threading.Thread(target=trigger)
    thread.start()
    if action == "leader_exit":
        prepared().run(directory=directory, executable=engine)
    else:
        with pytest.raises(KeyboardInterrupt if action == "interrupt" else fschem.SimulationError):
            prepared().run(directory=directory, executable=engine,
                           timeout=2 if action == "timeout" else 5, cancel=cancel)
    thread.join()
    expected = {"timeout": "timed_out", "cancel": "cancelled", "interrupt": "cancelled", "leader_exit": "succeeded"}[action]
    assert fschem.Run.open(directory).status == expected
    before = (directory / "heartbeat").read_text()
    time.sleep(.15)
    assert (directory / "heartbeat").read_text() == before


@pytest.mark.parametrize("backend", ["ngspice", "vacask"])
def test_real_native_divider(tmp_path, backend):
    if not shutil.which(backend):
        pytest.skip(f"{backend} not installed")
    home = tmp_path / "home"
    home.mkdir()
    run = prepared(backend).run(directory=tmp_path / "run", timeout=30, env={"HOME": str(home)})
    results = run.results
    assert len(results) == (1 if backend == "ngspice" else 3)
    for result in results.values():
        if result.name == "Operating Point":
            np.testing.assert_allclose(result.voltage("vout" if backend == "ngspice" else "VOUT"), [2])
    assert fschem.Run.open(run.directory).status == "succeeded"


def test_ngspice_nested_paths_local_startup_and_osdi(tmp_path):
    source_dir = tmp_path / 'source with spaces'
    source_dir.mkdir()
    models = source_dir / 'model files'
    models.mkdir()
    (models / 'inner.inc').write_text('.param supply=3\n')
    (models / 'outer.inc').write_text('.include "inner.inc"\n')
    (models / 'sections.lib').write_text('.lib TT\n.param extra=1\n.endl TT\n')
    (source_dir / '.spiceinit').write_text('set numdgt=9\n')
    text = (ROOT / 'examples/spice.sch').read_text().replace('S {}', 'S {.include "model files/outer.inc"\n.lib "model files/sections.lib" TT}')
    text = text.replace('name=V1 value=3}', 'name=V1 value={supply}}')
    source = source_dir / 'divider.sch'
    source.write_text(text)
    sim = fschem.Simulation(fschem.Project(source_dir).load(source), mode='native')
    p = sim.prepare(backend='ngspice')
    assert p.files['.spiceinit'] == 'set numdgt=9\n'
    assert '.lib "inputs/0002.inc" TT' in p.deck
    assert '.lib TT' in p.files['inputs/0002.inc']
    if shutil.which('ngspice'):
        home = tmp_path / 'home'
        home.mkdir()
        run = p.run(directory=tmp_path / 'run', env={'HOME': str(home)}, timeout=30)
        np.testing.assert_allclose(run.result('Operating Point').voltage('vout'), [2])
    # Preparation resolves module paths without attempting to load/execute them.
    (models / 'module.osdi').write_bytes(b'fixture')
    (source_dir / '.spiceinit').write_text('osdi "model files/module.osdi"\n')
    p = sim.prepare(backend='ngspice')
    assert str(models / 'module.osdi') in p.files['.spiceinit']
    (source_dir / '.spiceinit').write_text('osdi "$PDK/module.osdi"\n')
    with pytest.raises(ValueError, match='dynamic native path'):
        sim.prepare(backend='ngspice')


def test_real_ngspice_control_multi_analysis(tmp_path):
    if not shutil.which('ngspice'):
        pytest.skip('ngspice not installed')
    source = tmp_path / 'multi.sch'
    source.write_text((ROOT / 'examples/spice.sch').read_text().replace('value=".op"', 'value=".control\nop\ntran 1n 10n\n.endc"'))
    home = tmp_path / 'home'
    home.mkdir()
    sim = fschem.Simulation(fschem.Project(tmp_path).load(source), mode='native')
    run = sim.run(backend='ngspice', directory=tmp_path / 'run', env={'HOME': str(home)}, timeout=30)
    assert list(run.results) == ['analysis-0000.raw#0', 'analysis-0001.raw#0']
    np.testing.assert_allclose(run.result('Operating Point').voltage('vout'), [2])
    assert len(run.result('Transient Analysis').time) > 1


def test_vacask_merged_local_config_and_relative_include(tmp_path):
    from fschem._native import tomllib
    source_dir = tmp_path / 'source with spaces'
    source_dir.mkdir()
    (source_dir / 'models').mkdir()
    (source_dir / 'models' / 'supply.inc').write_text('parameters supply=3\n')
    (source_dir / '.vacaskrc.toml').write_text('[Paths]\ninclude_path_prefix=["models"]\nmodule_path_suffix=["modules"]\n')
    source = source_dir / 'divider.sch'
    source.write_text((ROOT / 'examples/vacask.sch').read_text().replace('E {}', 'E {include "supply.inc"}').replace('vacask_params="dc=3 mag=1"', 'vacask_params="dc=supply mag=1"'))
    p = fschem.Simulation(fschem.Project(source_dir).load(source), mode='native').prepare(backend='vacask')
    config = tomllib.loads(p.files['vacask.toml'])
    assert config['Paths']['include_path_prefix'] == [str(source_dir), str(source_dir / 'models')]
    assert config['Paths']['module_path_suffix'] == [str(source_dir / 'modules')]
    if shutil.which('vacask'):
        run = p.run(directory=tmp_path / 'run', timeout=30)
        assert run.manifest['execution']['argv'].count('--extra-tomlfile') == 1
        np.testing.assert_allclose(run.result('Operating Point').voltage('VOUT'), [2])


def test_reopen_rejects_escaping_paths_and_schema(tmp_path):
    run = prepared().write(tmp_path / 'run')
    data = run.manifest
    data['artifacts']['deck'] = '../outside'
    manifest = run.directory / 'run.json'
    manifest.write_text(json.dumps(data))
    with pytest.raises(ValueError, match='escapes'):
        fschem.Run.open(run.directory)
    data['schema_version'] = 999
    manifest.write_text(json.dumps(data))
    with pytest.raises(ValueError, match='schema_version'):
        fschem.Run.open(run.directory)


def test_native_control_flow_fails_closed(tmp_path):
    for commands, message in [('.control\nrepeat 2\nop\nend\n.endc', 'control flow'),
                              ('.op\n.control\ntran 1n 10n\n.endc', 'combine')]:
        source = tmp_path / 'unsupported.sch'
        source.write_text((ROOT / 'examples/spice.sch').read_text().replace('value=".op"', f'value="{commands}"'))
        sim = fschem.Simulation(fschem.Project(tmp_path).load(source), mode='native')
        with pytest.raises(ValueError, match=message):
            sim.prepare(backend='ngspice')


def test_duplicate_plot_names_remain_separate_and_missing_analyses_fail(tmp_path):
    code = ''.join(f"Path({name!r}).write_text({RAW!r})\n" for name in ('op1.raw', 'op2.raw', 'op3.raw'))
    run = prepared('vacask').run(directory=tmp_path / 'run', executable=fake(tmp_path, code))
    assert len(run.results) == 3
    assert run.result('op2').name == 'Operating Point'
    with pytest.raises(KeyError, match='ambiguous'):
        run.result('Operating Point')
    with pytest.raises(fschem.SimulationError, match='incomplete'):
        prepared('vacask').run(directory=tmp_path / 'partial', executable=success(tmp_path))


def test_preexisting_raw_is_not_published(tmp_path):
    run = prepared().write(tmp_path / 'run')
    (run.directory / 'prior.raw').write_text(RAW)
    with pytest.raises(fschem.SimulationError, match='already contains'):
        run.execute(executable=success(tmp_path))
    assert run.manifest['results'] == []


def test_real_ngspice_dot_multi_analysis(tmp_path):
    if not shutil.which('ngspice'):
        pytest.skip('ngspice not installed')
    source = tmp_path / 'multi.sch'
    source.write_text((ROOT / 'examples/spice.sch').read_text().replace('value=".op"', 'value=".op\n.tran 1n 10n\n.ac dec 2 1 10"'))
    home = tmp_path / 'home'
    home.mkdir()
    sim = fschem.Simulation(fschem.Project(tmp_path).load(source), mode='native')
    run = sim.run(backend='ngspice', directory=tmp_path / 'run', env={'HOME': str(home)}, timeout=30)
    assert len(run.results) == 3
    assert run.result('Transient Analysis').time.size > 0
    assert run.result('AC Analysis').frequency.size > 0


def test_nonposix_run_refuses_before_artifacts(tmp_path, monkeypatch):
    from types import SimpleNamespace
    import fschem.simulation as implementation
    p = prepared()
    monkeypatch.setattr(implementation, 'os', SimpleNamespace(name='nt'))
    with pytest.raises(fschem.UnsupportedPlatform, match='POSIX'):
        p.run(directory=tmp_path / 'unsupported')
    assert not (tmp_path / 'unsupported').exists()


@pytest.mark.parametrize('output', ['simulator.log', '.run.json.tmp'])
def test_runner_output_symlinks_do_not_escape_or_launch(tmp_path, output):
    run = prepared().write(tmp_path / 'run')
    outside = tmp_path / 'outside'
    if output == '.run.json.tmp':
        outside.write_text('sentinel')
    (run.directory / output).symlink_to(outside)
    launched = tmp_path / 'launched'
    engine = fake(tmp_path, f'Path({str(launched)!r}).touch()\n')
    with pytest.raises(fschem.SimulationError):
        run.execute(executable=engine)
    assert not launched.exists()
    if output == '.run.json.tmp':
        assert outside.read_text() == 'sentinel'
    else:
        assert not outside.exists()
    assert json.loads((run.directory / 'run.json').read_text())['status'] == 'failed'
