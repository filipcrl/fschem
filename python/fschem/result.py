"""Owned NumPy results, retaining exact native signal and plot names."""
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Mapping

import numpy as np
from numpy.typing import NDArray

from . import _core
from .schematic import PathInput


@dataclass(frozen=True)
class CalculatedSignal:
    values: NDArray
    kind: str
    unit: str


class Result:
    """One raw plot. Arrays are copied from Rust, not zero-copy views.

    Signal lookup is exact: hierarchy separators are never rewritten. Voltage
    accepts a native node name and looks for v(node), then the exact node name.
    """

    def __init__(self, title: str, name: str, signals: Mapping[str, NDArray],
                 kinds: Mapping[str, str], scale_name: str | None, *,
                 native_kinds: Mapping[str, str] | None = None,
                 units: Mapping[str, str] | None = None):
        self.signal_map = MappingProxyType({name: name for name in signals})
        self.title = title
        self.name = name
        self.signals = MappingProxyType(dict(signals))
        self.kinds = MappingProxyType(dict(kinds))
        self.native_kinds = MappingProxyType(dict(native_kinds or kinds))
        self.units = MappingProxyType(dict(units or {name: _core.unit(kind) for name, kind in kinds.items()}))
        self.scale_name = scale_name

    def __getitem__(self, signal: str) -> NDArray:
        return self.signals[signal]

    @property
    def scale(self) -> NDArray | None:
        return None if self.scale_name is None else self[self.scale_name]

    @property
    def time(self) -> NDArray:
        if self.scale_name is None or not (self.kinds[self.scale_name].lower() == "time" or
                (self.scale_name == "time" and self.kinds[self.scale_name] == "notype"
                 and self.name == "Transient Analysis")):
            raise ValueError("plot does not have a time scale")
        return self[self.scale_name].real

    @property
    def frequency(self) -> NDArray:
        if self.scale_name is None or self.kinds[self.scale_name].lower() != "frequency":
            raise ValueError("plot does not have a frequency scale")
        return self[self.scale_name].real

    def calculate(self, expression: str) -> CalculatedSignal:
        """Evaluate + - * /, abs and deriv with units; never Python eval.

        v(OUT), i(VDD), v(/canonical/path), bare vector names, and backtick
        quoted exact native names are supported. Constants are dimensionless.
        """
        variables = [(name, self.kinds[name], values.real.tolist(), values.imag.tolist())
                     for name, values in self.signals.items()]
        for alias, native in self.signal_map.items():
            if alias not in self.signals:
                values = self.signals[native]
                variables.append((alias, self.kinds[native], values.real.tolist(), values.imag.tolist()))
        names = list(self.signals)
        scale = None if self.scale_name is None else names.index(self.scale_name)
        kind, unit, real, imaginary = _core.calculate(expression, variables, scale)
        values = np.asarray(real, dtype=np.float64)
        if any(np.iscomplexobj(v) for v in self.signals.values()):
            values = values + 1j * np.asarray(imaginary, dtype=np.float64)
        return CalculatedSignal(values, kind, unit)

    def voltage(self, node: str) -> NDArray:
        if node.startswith("/") and node in self.signal_map:
            return self[self.signal_map[node]]
        wrapped = f"v({node})"
        return self[wrapped if wrapped in self.signals else node]


def read_raw(path: PathInput) -> tuple[Result, ...]:
    """Read all concatenated ASCII/binary real/complex ngspice/VACASK raw plots.

    Parser limits (256 MiB, 16 million samples) are inherited from Rust.
    Malformed input raises ValueError; no partially parsed results are returned.
    """
    results = []
    for title, name, complex_, scale, variables in _core.read_raw(Path(path)):
        signals = {}
        kinds = {}
        native_kinds = {}
        units = {}
        for signal, kind, native_kind, unit, real, imaginary in variables:
            if signal in signals:
                raise ValueError(f"duplicate raw signal name: {signal}")
            values = np.asarray(real, dtype=np.float64)
            if complex_:
                values = values + 1j * np.asarray(imaginary, dtype=np.float64)
            signals[signal] = values
            kinds[signal] = kind
            native_kinds[signal] = native_kind
            units[signal] = unit
        scale_name = variables[scale][0] if scale is not None else None
        results.append(Result(title, name, signals, kinds, scale_name,
                              native_kinds=native_kinds, units=units))
    return tuple(results)
