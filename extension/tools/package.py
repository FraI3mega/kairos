#!/usr/bin/env python3
"""Merges manifest.json with manifest.<target>.json into dist/<target>/ and zips that folder."""
import json
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def merge(base, extra):
    for key, value in extra.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            merge(base[key], value)
        else:
            base[key] = value
    return base


def referenced(manifest):
    files = set(manifest.get("icons", {}).values())
    files.add(manifest["action"]["default_popup"])
    files.update(manifest["action"]["default_icon"].values())
    bg = manifest["background"]
    files.update(bg.get("scripts", []))
    if "service_worker" in bg:
        files.add(bg["service_worker"])
    return files


def main(target):
    out = ROOT / "dist" / target
    manifest = merge(
        json.loads((ROOT / "manifest.json").read_text()),
        json.loads((ROOT / f"manifest.{target}.json").read_text()),
    )
    missing = sorted(f for f in referenced(manifest) if not (out / f).is_file())
    if missing:
        sys.exit(f"{target}: manifest references missing files: {', '.join(missing)}")
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

    archive = ROOT / "dist" / f"kairos-{target}-{manifest['version']}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
        for path in sorted(out.rglob("*")):
            if path.is_file():
                info = zipfile.ZipInfo(path.relative_to(out).as_posix(), date_time=(2026, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = 0o644 << 16
                z.writestr(info, path.read_bytes())


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in ("chrome", "firefox"):
        sys.exit("usage: package.py chrome|firefox")
    main(sys.argv[1])
