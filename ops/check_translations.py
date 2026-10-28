#!/usr/bin/env python3
"""Check matching nonempty RU/EN catalogues and format placeholders."""
from pathlib import Path
import json
import re

root = Path(__file__).resolve().parents[1]
for ru, en in [(root / "locales/bot.ru.json", root / "locales/bot.en.json"), (root / "frontend/src/shared/i18n/messages/ru.json", root / "frontend/src/shared/i18n/messages/en.json")]:
    a, b = json.loads(ru.read_text()), json.loads(en.read_text())
    def flatten(value, prefix=""):
        result = {}
        for key, text in value.items():
            full = prefix + key
            if isinstance(text, dict): result.update(flatten(text, full + "."))
            else: result[full] = text
        return result
    a, b = flatten(a), flatten(b)
    assert a.keys() == b.keys(), f"Translation keys differ: {set(a) ^ set(b)}"
    for key in a:
        assert isinstance(a[key], str) and a[key].strip(), f"Empty RU translation: {key}"
        assert isinstance(b[key], str) and b[key].strip(), f"Empty EN translation: {key}"
        assert sorted(re.findall(r"\{[a-zA-Z0-9_]+\}", a[key])) == sorted(re.findall(r"\{[a-zA-Z0-9_]+\}", b[key])), f"Placeholder mismatch: {key}"
    print(f"{ru.parent.relative_to(root)}: {len(a)} matching RU/EN translations")
