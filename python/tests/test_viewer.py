import json
import subprocess

import pytest

from fschem import viewer


def test_open_run_uses_argument_list_without_shell(tmp_path, monkeypatch):
    directory = tmp_path / "run with spaces;not-a-command"
    directory.mkdir()
    manifest = directory / "run.json"
    manifest.write_text(json.dumps({"schema_version": 1}))
    calls = []
    handle = object()

    def launch(*args, **kwargs):
        calls.append((args, kwargs))
        return handle

    monkeypatch.setattr(subprocess, "Popen", launch)
    assert viewer.open_run(directory, executable="/custom gui/fschem") is handle
    assert calls == [((["/custom gui/fschem", "--open-run", str(manifest)],), {})]
    assert viewer.open_run(manifest) is handle
    with pytest.raises(FileNotFoundError):
        viewer.open_run(directory / "absent")
    with pytest.raises(ValueError):
        viewer.open_run(tmp_path)


def test_import_does_not_launch(monkeypatch):
    import importlib
    monkeypatch.setattr(subprocess, "Popen", lambda *a, **k: pytest.fail("launched on import"))
    importlib.reload(viewer)
