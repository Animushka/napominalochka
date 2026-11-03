#!/usr/bin/env python3
"""Guard backend layer references; regression fixtures may compose real adapters."""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
ALLOWED = {
    "domain": {"domain"},
    "application": {"domain", "application"},
    "adapters": {"domain", "application", "adapters"},
    "infrastructure": {"domain", "application", "infrastructure"},
    "bootstrap": {"domain", "application", "adapters", "infrastructure", "bootstrap"},
}
FRAMEWORKS = re.compile(r"\b(?:axum|teloxide|reqwest|rusqlite|aes_gcm)::")
# Strip Rust comments and string literals before inspecting references. Raw literals
# include embedded HTML/JSON and must not be mistaken for Rust dependencies.
NON_CODE = re.compile(
    r'r(?P<hashes>\#*)".*?"(?P=hashes)|"(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/',
    re.S,
)
REFERENCES = re.compile(r"\b(crate|self|super)((?:::[A-Za-z_][A-Za-z_0-9]*)+)")
errors = []
checked = 0
for path in sorted(SRC.rglob("*.rs")):
    relative = path.relative_to(SRC)
    if "tests" in relative.parts or path.stem == "tests" or path.stem.endswith("_tests"):
        continue
    if relative in {Path("main.rs"), Path("lib.rs")}:
        continue
    layer = relative.parts[0]
    if layer not in ALLOWED:
        errors.append(f"{relative}: production source must belong to a backend layer")
        continue
    code = NON_CODE.sub(" ", path.read_text())
    module = list(relative.with_suffix("").parts)
    if module[-1] == "mod":
        module.pop()
    for match in REFERENCES.finditer(code):
        prefix, suffix = match.groups()
        resolved = [] if prefix == "crate" else module.copy()
        parts = (["super"] if prefix == "super" else []) + suffix[2:].split("::")
        for part in parts:
            if part == "super":
                if resolved:
                    resolved.pop()
            else:
                resolved.append(part)
        if resolved and resolved[0] not in ALLOWED[layer]:
            errors.append(f"{relative}: forbidden dependency {match.group()}")
    if re.search(r"\buse\s+crate::\{", code):
        errors.append(f"{relative}: spell root imports with explicit layer paths")
    if layer in {"domain", "application"} and FRAMEWORKS.search(code):
        errors.append(f"{relative}: transport, encryption and SQL implementations belong in adapters")
    # An explicit module API makes dependencies reviewable; test globs/preludes are idiomatic.
    if re.search(r"\buse\s+(?:super|self|crate)::[^;]*\*", code):
        errors.append(f"{relative}: import project symbols explicitly")
    checked += 1
if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print(f"Backend architecture: {checked} production files respect layer boundaries")
