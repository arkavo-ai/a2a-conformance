#!/usr/bin/env python3
"""Regenerates SCENARIOS.md from the scenario corpus. Run from the repo root."""
import json, glob, collections

groups = collections.OrderedDict()
for path in sorted(glob.glob("scenarios/*/*.json")):
    s = json.load(open(path))
    groups.setdefault(s["id"].split("/")[0], []).append(s)

lines = ["# Scenario catalog", "",
         "Generated view of `scenarios/` — the JSON files are the source of truth.",
         "Regenerate this file with `python3 tools/render_scenarios_md.py` after editing the corpus.", ""]
for g in ["core", "streaming", "errors", "discovery", "edge"]:
    lines.append(f"## {g} ({len(groups[g])})")
    lines.append("")
    lines.append("| ID | Title | Spec | Op | Expects |")
    lines.append("|---|---|---|---|---|")
    for s in groups[g]:
        e = s["expect"]
        if e["kind"] == "error":
            exp = f"error `{e['errorCode']}`"
        elif e["kind"] == "stream":
            exp = "stream: " + " → ".join(e.get("streamOrder") or [])
        else:
            exp = e["kind"]
        extra = " + request check" if "expectRequest" in s else ""
        lines.append(f"| `{s['id']}` | {s['title']} | {s['spec']} | `{s['client']['op']}` | {exp}{extra} |")
    lines.append("")
open("SCENARIOS.md", "w").write("\n".join(lines))
print("ok")
