"""Explicit technology dependencies, not a universal model converter."""
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class IHP:
    """Existing SG13G2 LV MOS wrappers in an installed IHP-Open-PDK tree.

    No model files are copied or modified. root points at IHP-Open-PDK (not
    ihp-sg13g2). Only deterministic LV corners are currently supported.
    """
    root: Path

    def __post_init__(self):
        object.__setattr__(self, "root", Path(self.root).resolve())

    def dependencies(self, backend, corner):
        if corner not in {"tt", "ss", "ff", "sf", "fs"}:
            raise ValueError("IHP LV corner must be tt, ss, ff, sf or fs")
        base = self.root / "ihp-sg13g2/libs.tech" / backend
        models = base / "models"
        library = models / "cornerMOSlv.lib"
        module = base / "osdi/psp103.osdi"
        nqs = base / "osdi/psp103_nqs.osdi"
        for path in (library, module, nqs):
            if not path.is_file():
                raise ValueError(f"missing IHP dependency: {path}")
            if any(c in str(path) for c in '\"\n\r$'):
                raise ValueError("unsupported technology path")
        if backend == "ngspice":
            if any(c.isspace() for c in str(base)):
                raise ValueError("ngspice OSDI paths containing whitespace are unsupported")
            return (f'.lib "{library}" mos_{corner}\n',
                    {".spiceinit": f'set ngbehavior=hsa\nset sourcepath = ( "{models}" )\nosdi {module}\nosdi {nqs}\n'}, {})
        return (f'parameters swsoa=0\nload "{module}"\nload "{nqs}"\ninclude "{library}" section=mos_{corner}\n', {},
                {"include_path_prefix": [str(models)], "module_path_prefix": [str(base / "osdi")]})
