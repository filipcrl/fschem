"""Owned NumPy results, retaining exact native signal and plot names."""
from pathlib import Path
from types import MappingProxyType
from typing import Mapping

import numpy as np
from numpy.typing import NDArray

from . import _core
from .schematic import PathInput


class Result:
    """One raw plot. Arrays are copied from Rust, not zero-copy views.

    Signal lookup is exact: hierarchy separators are never rewritten. Voltage
    accepts a native node name and looks for v(node), then the exact node name.
    """

    def __init__(self, title: str, name: str, signals: Mapping[str, NDArray],
                 kinds: Mapping[str, str], scale_name: str | None):
        self.title = title
        self.name = name
        self.signals = MappingProxyType(dict(signals))
        self.kinds = MappingProxyType(dict(kinds))
        self.scale_name = scale_name

    def __getitem__(self, signal: str) -> NDArray:
        return self.signals[signal]

    @property
    def scale(self) -> NDArray | None:
        return None if self.scale_name is None else self[self.scale_name]

    @property
    def time(self) -> NDArray:
        if self.scale_name is None or self.kinds[self.scale_name].lower() != "time":
            raise ValueError("plot does not have a time scale")
        return self[self.scale_name].real

    @property
    def frequency(self) -> NDArray:
        if self.scale_name is None or self.kinds[self.scale_name].lower() != "frequency":
            raise ValueError("plot does not have a frequency scale")
        return self[self.scale_name].real

    def voltage(self, node: str) -> NDArray:
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
        for signal, kind, real, imaginary in variables:
            if signal in signals:
                raise ValueError(f"duplicate raw signal name: {signal}")
            values = np.asarray(real, dtype=np.float64)
            if complex_:
                values = values + 1j * np.asarray(imaginary, dtype=np.float64)
            signals[signal] = values
            kinds[signal] = kind
        scale_name = variables[scale][0] if scale is not None else None
        results.append(Result(title, name, signals, kinds, scale_name))
    return tuple(results)
