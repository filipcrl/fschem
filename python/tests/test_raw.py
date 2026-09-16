import struct

import numpy as np
import pytest

from fschem import read_raw


def header(name="Transient Analysis", complex_=False, binary=False):
    return (f"Title: fixture\nPlotname: {name}\nFlags: {'complex' if complex_ else 'real'}\n"
            "No. Variables: 2\nNo. Points: 2\nVariables:\n"
            f"0 {'frequency frequency' if complex_ else 'time time'}\n"
            "1 v(top:out) voltage\n" + ("Binary:\n" if binary else "Values:\n")).encode()


@pytest.mark.parametrize("binary", [False, True])
@pytest.mark.parametrize("complex_", [False, True])
def test_numpy_raw_plots(tmp_path, binary, complex_):
    path = tmp_path / "result.raw"
    values = [0, 0, 1, -2, 1, 0, 2, -3] if complex_ else [0, 1, 1, 2]
    if binary:
        payload = struct.pack("=" + "d" * len(values), *values)
    elif complex_:
        payload = b"0 0,0 1,-2\n1 1,0 2,-3\n"
    else:
        payload = b"0 0 1\n1 1 2\n"
    plot = header(complex_=complex_, binary=binary) + payload
    path.write_bytes(plot + b"\n" + plot)
    results = read_raw(path)
    assert len(results) == 2
    result = results[0]
    assert result.title == "fixture"
    assert result.name == "Transient Analysis"
    assert tuple(result.signals) == (("frequency" if complex_ else "time"), "v(top:out)")
    expected = [1 - 2j, 2 - 3j] if complex_ else [1, 2]
    np.testing.assert_array_equal(result.voltage("top:out"), expected)
    assert result.voltage("top:out").dtype == (np.complex128 if complex_ else np.float64)
    np.testing.assert_array_equal(result.frequency if complex_ else result.time, [0, 1])
    with pytest.raises(ValueError):
        _ = result.time if complex_ else result.frequency
    with pytest.raises(KeyError):
        result.voltage("top.out")  # no blind hierarchy separator aliases
    with pytest.raises(TypeError):
        result.signals["new"] = np.zeros(2)


def test_operating_point_and_bad_input(tmp_path):
    path = tmp_path / "op.raw"
    path.write_bytes(header(name="Operating Point") + b"0 0 1\n1 1 2\n")
    result, = read_raw(path)
    assert result.scale is None
    with pytest.raises(ValueError, match="time scale"):
        _ = result.time
    for data in [b"", b"not a raw file", header(binary=True) + b"short"]:
        path.write_bytes(data)
        with pytest.raises(ValueError):
            read_raw(path)
    with pytest.raises(ValueError):
        read_raw(tmp_path / "missing.raw")


def test_duplicate_signal_names_fail_without_losing_data(tmp_path):
    path = tmp_path / "duplicate.raw"
    path.write_bytes(header().replace(b"v(top:out) voltage", b"time voltage") + b"0 0 1\n1 1 2\n")
    with pytest.raises(ValueError, match="duplicate raw signal"):
        read_raw(path)
