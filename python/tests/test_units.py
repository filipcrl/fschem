from types import MappingProxyType
import numpy as np
import pytest
from fschem.result import Result
from fschem import read_raw


def result():
    r = Result("units", "Transient Analysis",
               {"time": np.array([0., 1., 2.]), "OUT": np.array([2., 4., 6.]),
                "VDD:flow(br)": np.array([.001, .002, .003])},
               {"time": "time", "OUT": "voltage", "VDD:flow(br)": "current"}, "time")
    r.signal_map = MappingProxyType({"/OUT": "OUT"})
    return r


def test_derived_units_and_aliases():
    r = result()
    assert r.calculate("v(OUT)/v(/OUT)").unit == "V/V"
    resistance = r.calculate("v(OUT)/i(VDD)")
    assert resistance.unit == "Ω"
    np.testing.assert_allclose(resistance.values, 2000.)
    assert r.calculate("v(OUT)*i(VDD)").unit == "W"
    derivative = r.calculate("deriv(v(OUT))")
    assert derivative.unit == "V/s"
    np.testing.assert_allclose(derivative.values, 2.)
    np.testing.assert_allclose(r.calculate("abs(-`OUT`)").values, r["OUT"])
    with pytest.raises(ValueError, match="incompatible units"):
        r.calculate("v(OUT)+i(VDD)")
    with pytest.raises(ValueError):
        r.calculate("__import__('os')")
    with pytest.raises(ValueError, match="nested derivatives"):
        r.calculate("deriv(deriv(v(OUT)))")


def test_vacask_types_and_native_metadata(tmp_path):
    p = tmp_path / "signals.raw"
    p.write_text("Title: test\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 3\nNo. Points: 2\nVariables:\n0 time notype\n1 OUT notype\n2 VDD:flow(br) notype\nValues:\n0 0\n1\n.001\n1 1\n2\n.002\n")
    r, = read_raw(p)
    assert dict(r.units) == {"time": "s", "OUT": "V", "VDD:flow(br)": "A"}
    assert set(r.native_kinds.values()) == {"notype"}
    np.testing.assert_array_equal(r.time, [0, 1])
    assert r.calculate("v(OUT)/i(VDD)").unit == "Ω"
