"""Writes THIRD-PARTY-LICENSES.md for the crates compiled into buku.exe on Windows.

Run after changing dependencies:
    cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc --locked > metadata.json
    python installer/third_party_licenses.py metadata.json THIRD-PARTY-LICENSES.md

Reads `cargo metadata` (Windows target) from argv[1], follows normal dependencies
from the root (not dev or build ones, which don't ship), and copies each crate's
license files from its source folder. Identical texts are printed once, with the
crates that use them listed above.
"""
import json
import pathlib
import sys

meta = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
out = pathlib.Path(sys.argv[2])
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
root = meta["resolve"]["root"]

shipped, stack = set(), [root]
while stack:
    node = nodes[stack.pop()]
    for dep in node["deps"]:
        kinds = {k["kind"] for k in dep["dep_kinds"]}
        if None in kinds and dep["pkg"] not in shipped:  # a normal dependency
            shipped.add(dep["pkg"])
            stack.append(dep["pkg"])

LICENSE_NAMES = ("license", "copying", "unlicense", "notice")
texts = {}  # text -> list of "name version"
summary = []
for pid in sorted(shipped, key=lambda i: (packages[i]["name"], packages[i]["version"])):
    pkg = packages[pid]
    folder = pathlib.Path(pkg["manifest_path"]).parent
    files = sorted(
        f for f in folder.iterdir()
        if f.is_file() and f.name.lower().startswith(LICENSE_NAMES)
    )
    label = f'{pkg["name"]} {pkg["version"]}'
    summary.append((label, pkg.get("license") or "see files", pkg.get("repository") or ""))
    if not files:
        texts.setdefault(f'(No license file in the package; declared license: {pkg.get("license")})', []).append(label)
    for f in files:
        text = f.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n").strip()
        texts.setdefault(text, []).append(f"{label} ({f.name})")

lines = [
    "# Third-party licenses",
    "",
    "BUKU's program file is built from the Rust crates below, under their own",
    "licenses, including the code generators that run while it is compiled. Crates",
    "used only by its tests or to add its icon are not listed.",
    "",
    "| Crate | Version | License |",
    "|---|---|---|",
]
for label, lic, repo in summary:
    name, version = label.rsplit(" ", 1)
    lines.append(f"| {name} | {version} | {lic} |")
lines.append("")
for text, users in texts.items():
    lines.append("---")
    lines.append("")
    lines.append("Used by: " + ", ".join(users))
    lines.append("")
    lines.append("```text")
    lines.append(text.replace("```", "'''"))
    lines.append("```")
    lines.append("")
out.write_text("\n".join(lines), encoding="utf-8", newline="\n")
print(len(shipped), "crates,", len(texts), "distinct license texts")
