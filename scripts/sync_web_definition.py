#!/usr/bin/env python3
"""Embed the canonical web runner into the standalone service JSON."""
import json
from pathlib import Path

folder = Path(__file__).resolve().parents[1] / "crates/server/definitions"
path = folder / "http-static.json"
definition = json.loads(path.read_text())
definition["version"] = "5"
for command in (definition["runtime"]["command"], definition["prepare"]):
    command[-1] = "{_tls}"
definition["files"][0]["content"] = (folder / "web-run.py").read_text()
path.write_text(json.dumps(definition, indent=2, ensure_ascii=False) + "\n")
