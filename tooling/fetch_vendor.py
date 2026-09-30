#!/usr/bin/env python3
"""Fetch the third-party files listed in static/vendor.toml into static/vendor/.

Nix builds do this themselves; this is for building with a plain `cargo`.
Every tarball is checked against the sha512 integrity pinned in the manifest
before anything is extracted, and static/vendor/ is replaced only once every
package has been fetched and verified.

    python3 tooling/fetch_vendor.py          fetch, unless already up to date
    python3 tooling/fetch_vendor.py --force  fetch regardless
"""

from __future__ import annotations

import base64
import hashlib
import io
import pathlib
import shutil
import sys
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "static" / "vendor.toml"
TARGET = ROOT / "static" / "vendor"
# A copy of the manifest the files were fetched for. The build compares it with
# the current manifest to catch a stale static/vendor/.
STAMP = "vendor.toml"


def verified_tarball(package: dict[str, object]) -> bytes:
    url = str(package["url"])
    algorithm, _, expected = str(package["integrity"]).partition("-")
    if algorithm != "sha512":
        raise SystemExit(f"{package['name']}: only sha512 integrity is supported")
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    actual = base64.b64encode(hashlib.sha512(data).digest()).decode()
    if actual != expected:
        raise SystemExit(
            f"{package['name']}: {url} does not match the pinned integrity\n"
            f"  expected sha512-{expected}\n  received sha512-{actual}"
        )
    return data


def extract(package: dict[str, object], data: bytes, into: pathlib.Path) -> None:
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        members = {member.name: member for member in archive.getmembers()}

        def write(name: str, destination: pathlib.Path) -> None:
            member = members.get(name)
            if member is None or not member.isfile():
                raise SystemExit(f"{package['name']}: tarball has no file {name}")
            source = archive.extractfile(member)
            assert source is not None
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(source.read())

        for entry in package["files"]:  # type: ignore[union-attr]
            source, target = entry["from"], entry["to"]
            suffix = entry.get("suffix")
            if suffix is None:
                write(source, into / target)
                continue
            matched = sorted(
                name
                for name, member in members.items()
                if member.isfile()
                and name.startswith(f"{source}/")
                and "/" not in name[len(source) + 1 :]
                and name.endswith(suffix)
            )
            if not matched:
                raise SystemExit(f"{package['name']}: no {suffix} files in {source}")
            for name in matched:
                write(name, into / target / name.rsplit("/", 1)[1])


def up_to_date(manifest: dict[str, object], manifest_bytes: bytes) -> bool:
    """The stamp matches and every listed file is still there."""
    stamp = TARGET / STAMP
    if not stamp.is_file() or stamp.read_bytes() != manifest_bytes:
        return False
    for package in manifest["package"]:  # type: ignore[union-attr]
        for entry in package["files"]:
            target = TARGET / entry["to"]
            suffix = entry.get("suffix")
            if suffix is None:
                if not target.is_file():
                    return False
            elif not target.is_dir() or not any(target.glob(f"*{suffix}")):
                return False
    return True


def main() -> None:
    manifest_bytes = MANIFEST.read_bytes()
    manifest = tomllib.loads(manifest_bytes.decode())
    if "--force" not in sys.argv[1:] and up_to_date(manifest, manifest_bytes):
        print(f"{TARGET.relative_to(ROOT)} is up to date")
        return
    with tempfile.TemporaryDirectory(dir=TARGET.parent) as staging_name:
        staging = pathlib.Path(staging_name) / "vendor"
        staging.mkdir()
        for package in manifest["package"]:
            print(f"fetching {package['name']} {package['version']}")
            extract(package, verified_tarball(package), staging)
        (staging / STAMP).write_bytes(manifest_bytes)
        if TARGET.exists():
            shutil.rmtree(TARGET)
        staging.rename(TARGET)
    print(f"wrote {TARGET.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
