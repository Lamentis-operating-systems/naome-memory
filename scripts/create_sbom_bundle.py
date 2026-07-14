#!/usr/bin/env python3
"""Bundle workspace CycloneDX JSON files into a reproducible tar.gz."""

from __future__ import annotations

import argparse
import gzip
import io
import pathlib
import tarfile


def included_boms(root: pathlib.Path) -> list[pathlib.Path]:
    return sorted(
        path
        for path in root.rglob("bom.json")
        if ".git" not in path.parts and "target" not in path.parts
    )


def create_bundle(root: pathlib.Path, output: pathlib.Path) -> None:
    root = root.resolve()
    boms = included_boms(root)
    if not boms:
        raise SystemExit("no CycloneDX bom.json files found")
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as compressed:
            with tarfile.open(
                fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT
            ) as archive:
                for path in boms:
                    payload = path.read_bytes()
                    relative = path.relative_to(root).as_posix()
                    member = tarfile.TarInfo(relative)
                    member.size = len(payload)
                    member.mode = 0o644
                    member.uid = 0
                    member.gid = 0
                    member.uname = "root"
                    member.gname = "root"
                    member.mtime = 0
                    archive.addfile(member, io.BytesIO(payload))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=".", type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args()
    create_bundle(args.root, args.output)


if __name__ == "__main__":
    main()
