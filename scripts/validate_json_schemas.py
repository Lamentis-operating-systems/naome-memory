#!/usr/bin/env python3
"""Validate committed Draft 2020-12 schemas and selected JSON projections."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

try:
    from jsonschema import Draft202012Validator
    from referencing import Registry, Resource
except ImportError as error:  # pragma: no cover - environment diagnostic
    raise SystemExit(
        "python package jsonschema is required for Draft 2020-12 validation"
    ) from error


def load_json(path: Path) -> object:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def validate_schema(path: Path) -> dict[str, object]:
    value = load_json(path)
    if not isinstance(value, dict):
        raise ValueError(f"{path}: schema root must be an object")
    if value.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
        raise ValueError(f"{path}: schema is not declared as Draft 2020-12")
    Draft202012Validator.check_schema(value)
    return value


def registry_for(schemas: dict[Path, dict[str, object]]) -> Registry:
    resources = []
    for path, schema in schemas.items():
        identifier = schema.get("$id")
        if not isinstance(identifier, str) or not identifier:
            raise ValueError(f"{path}: schema must declare a non-empty $id")
        resources.append((identifier, Resource.from_contents(schema)))
    return Registry().with_resources(resources)


def parse_binding(value: str) -> tuple[Path, Path]:
    schema, separator, instance = value.partition("=")
    if not separator or not schema or not instance:
        raise argparse.ArgumentTypeError("binding must be SCHEMA=INSTANCE")
    return Path(schema), Path(instance)


def load_binding_manifest(path: Path) -> list[tuple[Path, Path]]:
    value = load_json(path)
    if not isinstance(value, dict) or set(value) != {"contract_version", "bindings"}:
        raise ValueError(f"{path}: binding manifest has an invalid root shape")
    if value.get("contract_version") != "schema-fixture-bindings-v1":
        raise ValueError(f"{path}: unsupported binding manifest version")
    raw_bindings = value.get("bindings")
    if not isinstance(raw_bindings, list) or not raw_bindings:
        raise ValueError(f"{path}: bindings must be a non-empty array")
    bindings = []
    for index, raw in enumerate(raw_bindings):
        if not isinstance(raw, dict) or set(raw) != {"schema", "instance"}:
            raise ValueError(f"{path}: binding {index} has an invalid shape")
        schema = raw.get("schema")
        instance = raw.get("instance")
        if not isinstance(schema, str) or not isinstance(instance, str):
            raise ValueError(f"{path}: binding {index} paths must be strings")
        schema_path = Path(schema)
        instance_path = Path(instance)
        if (
            schema_path.is_absolute()
            or instance_path.is_absolute()
            or ".." in schema_path.parts
            or ".." in instance_path.parts
        ):
            raise ValueError(f"{path}: binding {index} paths must be relative and contained")
        bindings.append((schema_path, path.parent / instance_path))
    return bindings


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--bind",
        action="append",
        default=[],
        type=parse_binding,
        metavar="SCHEMA=INSTANCE",
        help="validate one JSON instance against one committed schema",
    )
    parser.add_argument(
        "--bindings-manifest",
        action="append",
        default=[],
        type=Path,
        metavar="PATH",
        help="load typed fixture schema-instance bindings from a generated manifest",
    )
    parser.add_argument(
        "--require-typed-fixtures",
        action="store_true",
        help="require a generated instance for every schema marked x-naome-typed-fixture",
    )
    args = parser.parse_args()

    schema_paths = sorted(Path("schemas").glob("*.schema.json"))
    if not schema_paths:
        raise ValueError("no committed JSON schemas found")
    definition_paths = sorted(Path("schemas").glob("*.defs.json"))
    schemas = {
        path: validate_schema(path) for path in [*schema_paths, *definition_paths]
    }
    registry = registry_for(schemas)

    default_bindings = [
        (
            Path("schemas/dataset-manifest-v1.schema.json"),
            Path("datasets/manifest-v1.json"),
        )
    ]
    manifest_bindings = [
        binding
        for manifest in args.bindings_manifest
        for binding in load_binding_manifest(manifest)
    ]
    bindings = [*default_bindings, *args.bind, *manifest_bindings]
    if args.require_typed_fixtures:
        required = {
            path
            for path in schema_paths
            if schemas[path].get("x-naome-typed-fixture") is True
        }
        bound = {schema_path for schema_path, _ in bindings}
        missing = sorted(required - bound)
        if missing:
            rendered = ", ".join(str(path) for path in missing)
            raise ValueError(f"typed fixture bindings are missing: {rendered}")
    for schema_path, instance_path in bindings:
        schema = schemas.get(schema_path)
        if schema is None:
            raise ValueError(f"{schema_path}: binding does not name a committed schema")
        errors = sorted(
            Draft202012Validator(schema, registry=registry).iter_errors(
                load_json(instance_path)
            ),
            key=lambda error: tuple(str(part) for part in error.absolute_path),
        )
        if errors:
            for error in errors:
                location = "/".join(str(part) for part in error.absolute_path) or "<root>"
                print(
                    f"{instance_path}:{location}: {error.message}",
                    file=sys.stderr,
                )
            return 1

    print(
        json.dumps(
            {
                "contract_version": "schema-validation-result-v1",
                "draft": "2020-12",
                "schema_count": len(schema_paths),
                "definition_library_count": len(definition_paths),
                "instance_count": len(bindings),
                "status": "passed",
            },
            separators=(",", ":"),
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
