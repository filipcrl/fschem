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
    with pytest.raises(ValueError, match="explicit"):
        fschem.Simulation(circuit, mode="portable")


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
