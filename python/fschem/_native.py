"""Native execution preparation, not a portable circuit compiler."""
from pathlib import Path
import re
import shlex

import tomli_w
try:
    import tomllib
except ImportError:  # Python 3.10
    import tomli as tomllib

# Keep token boundaries so quoted paths and .lib section selectors survive.
_TOKEN = re.compile(r'''"[^"\n]*"|'[^'\n]*'|[^\s]+''')
_ANALYSES = {"op", "tran", "ac", "dc", "noise", "sp", "sens", "disto", "run"}


def ngspice_inputs(deck: str, source: Path):
    files = {}
    visited = {}

    def relocate(text, base):
        lines = []
        for line in text.splitlines():
            tokens = list(_TOKEN.finditer(line))
            if not tokens:
                lines.append(line)
                continue
            command = tokens[0].group().lower()
            # .lib SECTION opens a definition; .lib FILE SECTION loads a file.
            is_include = command in {".include", ".inc", "source"} or (command == ".lib" and len(tokens) >= 3)
            if is_include or command in {"osdi", "pre_osdi"}:
                if len(tokens) < 2:
                    raise ValueError(f"missing native path: {line}")
                token = tokens[1]
                value = shlex.split(token.group())[0]
                if any(c in value for c in "$`{}\n\r"):
                    raise ValueError(f"dynamic native path cannot be relocated: {value}")
                path = Path(value).expanduser()
                if not path.is_absolute():
                    path = base / path
                    if not path.exists():
                        path = source.parent / value
                path = path.resolve(strict=True)
                if is_include:
                    if path not in visited:
                        name = f"inputs/{len(visited):04d}.inc"
                        visited[path] = name
                        files[name] = relocate(path.read_text(), path.parent)
                    value = visited[path]
                else:
                    value = str(path)
                if '"' in value:
                    raise ValueError("native paths containing quotes are unsupported")
                line = line[:token.start()] + '"' + value + '"' + line[token.end():]
            lines.append(line)
        return "\n".join(lines) + "\n"

    deck = relocate(deck, source.parent)
    startup = source.parent / ".spiceinit"
    if startup.is_file():
        files[".spiceinit"] = relocate(startup.read_text(), source.parent)
    lines = deck.splitlines()
    endcs = [i for i, line in enumerate(lines) if line.strip().lower() == ".endc"]
    control = False
    count = 0
    dot_count = 0
    flow = False
    output = []
    for i, line in enumerate(lines):
        words = line.strip().lower().split()
        command = words[0] if words else ""
        if not control and command.startswith(".") and command[1:] in _ANALYSES - {"run"}:
            dot_count += 1
        if control and command in {"while", "dowhile", "foreach", "repeat", "if", "goto"}:
            flow = True
        if command == ".control":
            control = True
        if command == ".endc":
            if count and i == endcs[-1]:
                output.append("quit")
            control = False
        output.append(line)
        if control and command in _ANALYSES:
            output.append(f"write analysis-{count:04d}.raw")
            count += 1
    if count and dot_count:
        raise ValueError("native runner cannot combine dot-card and control analyses; use one style")
    if count and flow:
        raise ValueError("native runner cannot retain analyses inside control flow; use straight-line analyses")
    return "\n".join(output) + "\n", files, max(count, dot_count)


def vacask_config(source: Path):
    path = source.parent / ".vacaskrc.toml"
    config = tomllib.loads(path.read_text()) if path.is_file() else {}
    paths = config.setdefault("Paths", {})
    for key in ("include_path", "include_path_prefix", "include_path_suffix",
                "module_path", "module_path_prefix", "module_path_suffix"):
        if key in paths:
            if not isinstance(paths[key], list) or not all(isinstance(p, str) for p in paths[key]):
                raise ValueError(f"VACASK Paths.{key} must be a string array")
            paths[key] = [str((source.parent / p).resolve()) for p in paths[key]]
    for key in ("include_path_prefix", "module_path_prefix"):
        paths[key] = [str(source.parent)] + paths.get(key, [])
    return tomli_w.dumps(config)
