#!/usr/bin/env python3
"""Create a byte-reproducible tar.gz containing the NAOME Memory CLI."""

from __future__ import annotations

import argparse
import gzip
import io
import pathlib
import tarfile


def create_archive(binary: pathlib.Path, output: pathlib.Path) -> None:
    payload = binary.read_bytes()
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw:
        with gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0
        ) as compressed:
            with tarfile.open(
                fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT
            ) as archive:
                member = tarfile.TarInfo("naome-memory")
                member.size = len(payload)
                member.mode = 0o755
                member.uid = 0
                member.gid = 0
                member.uname = "root"
                member.gname = "root"
                member.mtime = 0
                archive.addfile(member, io.BytesIO(payload))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args()
    create_archive(args.binary, args.output)


if __name__ == "__main__":
    main()
