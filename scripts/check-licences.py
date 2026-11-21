#!/usr/bin/env python3
"""Fails if any dependency would need a licence GlobalObject has chosen not to accept.

The rule: permissive licences only, plus MPL-2.0 for the packages listed below, used unmodified.
An expression passes if any one of its OR alternatives is acceptable.
"""
import json, re, subprocess, sys

PERMISSIVE = {
    "MIT", "MIT-0", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "0BSD",
    "Unlicense", "CC0-1.0", "BSL-1.0", "Unicode-3.0", "Unicode-DFS-2016", "CDLA-Permissive-2.0",
}
# Weak, file-level copyleft. Allowed unmodified, because nothing else provides these yet:
MPL_ALLOWED = {
    "attohttpc",   # through iroh's port mapping (UPnP, NAT-PMP)
    "option-ext",  # through rustic_core's use of `dirs`
}

def acceptable(term, name):
    term = re.sub(r"\s+WITH\s+.*$", "", term.strip(" ()"))
    return term in PERMISSIVE or (term == "MPL-2.0" and name in MPL_ALLOWED)

def check(expr, name):
    # "A/B" is an old way to write "A OR B". An AND needs every part accepted.
    for alternative in re.split(r"\s+OR\s+|/", expr):
        if all(acceptable(t, name) for t in re.split(r"\s+AND\s+", alternative)):
            return True
    return False

meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1"]))
used = {n["id"] for n in meta["resolve"]["nodes"]}
packages = [p for p in meta["packages"] if p["id"] in used]
bad = [(p["name"], p["version"], p.get("license") or "NONE") for p in packages if not check(p.get("license") or "", p["name"])]
for name, version, licence in sorted(bad):
    print(f"NOT ALLOWED: {name} {version}: {licence}")
print(f"{len(packages)} packages checked, {len(bad)} not allowed")
sys.exit(1 if bad else 0)
