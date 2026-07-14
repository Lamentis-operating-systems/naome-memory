#!/usr/bin/env python3
"""Fail unless the latest named GitHub Actions checks all succeeded."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--required", action="append", default=[])
    args = parser.parse_args()
    if not args.required:
        raise SystemExit("at least one --required check name is necessary")

    payload = json.loads(args.input.read_text(encoding="utf-8"))
    check_runs = payload.get("check_runs")
    if not isinstance(check_runs, list):
        raise SystemExit("GitHub check-runs response has no check_runs array")

    latest: dict[str, dict[str, object]] = {}
    for value in check_runs:
        if not isinstance(value, dict) or not isinstance(value.get("name"), str):
            continue
        name = value["name"]
        current = latest.get(name)
        if current is None or int(value.get("id", 0)) > int(current.get("id", 0)):
            latest[name] = value

    failures: list[str] = []
    for name in args.required:
        value = latest.get(name)
        if value is None:
            failures.append(f"{name}: missing")
            continue
        app = value.get("app")
        app_slug = app.get("slug") if isinstance(app, dict) else None
        if (
            value.get("status") != "completed"
            or value.get("conclusion") != "success"
            or app_slug != "github-actions"
        ):
            failures.append(
                f"{name}: status={value.get('status')} "
                f"conclusion={value.get('conclusion')} app={app_slug}"
            )
    if failures:
        raise SystemExit("required release checks failed: " + "; ".join(failures))

    print(
        json.dumps(
            {
                "contract_version": "release-check-verification-v1",
                "required": args.required,
                "status": "verified",
            },
            separators=(",", ":"),
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
