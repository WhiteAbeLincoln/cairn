# /// script
# requires-python = ">=3.12"
# dependencies = ["genson>=1.3", "jsonschema>=4.23"]
# ///
"""Infer a JSON Schema from all Claude Code JSONL session files.

Discriminates by the top-level `type` field, producing a oneOf schema
with one branch per type value. Fields listed in ENUM_PATHS are collected
during scanning and emitted as string enums. Array items at paths listed
in SUB_DISCRIMINATORS are split into oneOf variants, with optional nesting.

    uv run _assets/scripts/infer_schema.py [-o schema.json] [--dir ~/.claude/projects]
"""

import argparse
import json
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from genson import SchemaBuilder


def find_jsonl_files(root: Path) -> list[Path]:
    return sorted(root.rglob("*.jsonl"))


# Dotpaths to collect as enums. Use * to traverse array items.
ENUM_PATHS: list[str] = [
    "userType",
    "message.type",
    "message.role",
]


@dataclass
class SubDiscriminator:
    """Split array items into oneOf by a discriminator field.

    array_path: dotpath to the array (e.g. "message.content")
    disc_field: the property on each item to discriminate by (e.g. "type")
    filter:     only apply to items matching these field=value pairs
    children:   nested discriminators applied within each variant
    """
    array_path: str
    disc_field: str
    filter: dict[str, str] | None = None
    children: list["SubDiscriminator"] = field(default_factory=list)


SUB_DISCRIMINATORS: list[SubDiscriminator] = [
    SubDiscriminator(
        array_path="message.content",
        disc_field="type",
        children=[
            SubDiscriminator(
                array_path="message.content",
                disc_field="name",
                filter={"type": "tool_use"},
            ),
        ],
    ),
]


@dataclass
class SubCollected:
    """Collected schemas and counts for one sub-discriminator."""
    builders: dict[str, SchemaBuilder] = field(default_factory=lambda: defaultdict(SchemaBuilder))
    counts: dict[str, int] = field(default_factory=lambda: defaultdict(int))


@dataclass
class ScanResult:
    type_schemas: dict[str, dict] = field(default_factory=dict)
    # enum_values keyed by top-level type, then dotpath -> set of values
    enum_values: dict[str, dict[str, set[str]]] = field(default_factory=dict)
    # sub_collected keyed by top-level type, then disc id -> collected
    sub_collected: dict[str, dict[int, SubCollected]] = field(default_factory=dict)


def _collect_enum_values(obj: Any, path_parts: list[str], out: set[str]) -> None:
    """Walk obj following path_parts, collecting leaf string values into out."""
    if not path_parts:
        if isinstance(obj, str):
            out.add(obj)
        return
    key, *rest = path_parts
    if key == "*":
        if isinstance(obj, list):
            for item in obj:
                _collect_enum_values(item, rest, out)
    elif isinstance(obj, dict) and key in obj:
        _collect_enum_values(obj[key], rest, out)


def _resolve_path(obj: Any, path_parts: list[str]) -> list[Any]:
    """Walk obj following dotpath parts, returning all matching values."""
    if not path_parts:
        return [obj]
    key, *rest = path_parts
    if isinstance(obj, dict) and key in obj:
        return _resolve_path(obj[key], rest)
    return []


def _apply_enum(schema: dict, path_parts: list[str], values: list[str]) -> None:
    """Walk into a schema node following path_parts and set the leaf to an enum."""
    if not path_parts:
        return
    key, *rest = path_parts

    if key == "*":
        if items := schema.get("items"):
            _apply_enum(items, rest, values)
        return

    props = schema.get("properties", {})
    if key not in props:
        return

    if not rest:
        props[key] = {"type": "string", "enum": values}
    else:
        _apply_enum(props[key], rest, values)


def _set_items_oneof(schema: dict, path_parts: list[str], oneof_items: dict) -> None:
    """Walk into schema to an array property and replace its items with oneOf."""
    if not path_parts:
        return
    key, *rest = path_parts

    props = schema.get("properties", {})
    if key not in props:
        return

    if not rest:
        arr = props[key]
        if arr.get("type") == "array":
            arr["items"] = oneof_items
    else:
        _set_items_oneof(props[key], rest, oneof_items)


def _matches_filter(item: dict, filt: dict[str, str] | None) -> bool:
    if filt is None:
        return True
    return all(item.get(k) == v for k, v in filt.items())


def _collect_sub(obj: dict, disc: SubDiscriminator, type_subs: dict[int, SubCollected]) -> None:
    """Collect items for a sub-discriminator (and its children) from one event."""
    for arr in _resolve_path(obj, disc.array_path.split(".")):
        if not isinstance(arr, list):
            continue
        for item in arr:
            if not isinstance(item, dict):
                continue
            if not _matches_filter(item, disc.filter):
                continue
            disc_val = item.get(disc.disc_field, "__unknown__")
            coll = type_subs[id(disc)]
            coll.builders[disc_val].add_object(item)
            coll.counts[disc_val] += 1

            for child in disc.children:
                _collect_sub(obj, child, type_subs)


def _all_discs(discs: list[SubDiscriminator]) -> list[SubDiscriminator]:
    """Flatten all discriminators (including nested children)."""
    out = []
    for d in discs:
        out.append(d)
        out.extend(_all_discs(d.children))
    return out


def scan_files(
    files: list[Path],
    discriminator: str = "type",
) -> ScanResult:
    builders: dict[str, SchemaBuilder] = defaultdict(SchemaBuilder)
    counts: dict[str, int] = defaultdict(int)

    all_discs = _all_discs(SUB_DISCRIMINATORS)
    result = ScanResult()
    errors = 0

    def _ensure_type(tv: str) -> None:
        if tv not in result.enum_values:
            result.enum_values[tv] = {p: set() for p in ENUM_PATHS}
        if tv not in result.sub_collected:
            result.sub_collected[tv] = {id(d): SubCollected() for d in all_discs}

    for path in files:
        with path.open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    errors += 1
                    continue

                type_val = obj.get(discriminator, "__unknown__")
                builders[type_val].add_object(obj)
                counts[type_val] += 1

                _ensure_type(type_val)
                for dotpath in ENUM_PATHS:
                    _collect_enum_values(obj, dotpath.split("."), result.enum_values[type_val][dotpath])

                for disc in SUB_DISCRIMINATORS:
                    _collect_sub(obj, disc, result.sub_collected[type_val])

    # Report
    print(f"Scanned {len(files)} files, {sum(counts.values())} events, {errors} parse errors", file=sys.stderr)
    for t, c in sorted(counts.items(), key=lambda x: -x[1]):
        print(f"  {c:>8}  {t}", file=sys.stderr)

    print(f"\nEnum fields (per top-level type):", file=sys.stderr)
    for type_val, paths in sorted(result.enum_values.items()):
        for dotpath, vals in paths.items():
            if vals:
                print(f"  {type_val}.{dotpath}: {sorted(vals)}", file=sys.stderr)

    for disc in all_discs:
        filt_str = f" (where {disc.filter})" if disc.filter else ""
        print(f"\nSub-discriminator {disc.array_path}[].{disc.disc_field}{filt_str} (per top-level type):", file=sys.stderr)
        for type_val in sorted(result.sub_collected):
            coll = result.sub_collected[type_val][id(disc)]
            if not coll.counts:
                continue
            disc_vals = sorted(coll.counts.keys())
            print(f"  {type_val}: {disc_vals}", file=sys.stderr)

    result.type_schemas = {tv: b.to_schema() for tv, b in builders.items()}
    return result


def assemble_root_schema(per_type: dict[str, dict], discriminator: str = "type") -> dict:
    variants = []
    for type_val, schema in sorted(per_type.items()):
        schema.setdefault("properties", {})[discriminator] = {"const": type_val}
        if "required" in schema and discriminator not in schema["required"]:
            schema["required"].append(discriminator)
        variants.append(schema)

    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Claude Code Session Event",
        "oneOf": variants,
        "discriminator": {"propertyName": discriminator},
    }


def apply_enums(schema: dict, collected: dict[str, dict[str, set[str]]]) -> None:
    for variant in schema.get("oneOf", [schema]):
        type_val = variant.get("properties", {}).get("type", {}).get("const")
        if type_val not in collected:
            continue
        for dotpath, values in collected[type_val].items():
            if values:
                _apply_enum(variant, dotpath.split("."), sorted(values))


def _build_oneof_variants(
    coll: SubCollected, disc: SubDiscriminator, type_subs: dict[int, SubCollected],
) -> dict:
    """Build a oneOf schema from collected sub-discriminator data, applying children."""
    variants = []
    for disc_val, builder in sorted(coll.builders.items()):
        sub_schema = builder.to_schema()
        sub_schema.setdefault("properties", {})[disc.disc_field] = {"const": disc_val}
        if "required" in sub_schema and disc.disc_field not in sub_schema["required"]:
            sub_schema["required"].append(disc.disc_field)
        variants.append(sub_schema)

    # Apply children: find the matching variant and replace it with nested oneOf
    for child in disc.children:
        child_coll = type_subs[id(child)]
        if not child_coll.builders:
            continue
        child_oneof = _build_oneof_variants(child_coll, child, type_subs)

        # Find which variant(s) the child filter matches and replace
        new_variants = []
        for v in variants:
            props = v.get("properties", {})
            if child.filter and all(
                props.get(k, {}).get("const") == val for k, val in child.filter.items()
            ):
                for child_v in child_oneof["oneOf"]:
                    for k, val in (child.filter or {}).items():
                        child_v.setdefault("properties", {})[k] = {"const": val}
                    new_variants.append(child_v)
            else:
                new_variants.append(v)
        variants = new_variants

    return {
        "oneOf": variants,
        "discriminator": {"propertyName": disc.disc_field},
    }


def apply_sub_discriminators(schema: dict, result: ScanResult) -> None:
    """Replace merged array items with oneOf variants per top-level type."""
    for variant in schema.get("oneOf", [schema]):
        type_val = variant.get("properties", {}).get("type", {}).get("const")
        if type_val not in result.sub_collected:
            continue
        type_subs = result.sub_collected[type_val]

        for disc in SUB_DISCRIMINATORS:
            coll = type_subs[id(disc)]
            if not coll.builders:
                continue
            oneof_items = _build_oneof_variants(coll, disc, type_subs)
            _set_items_oneof(variant, disc.array_path.split("."), oneof_items)


def validate_schema(schema: dict, files: list[Path]) -> bool:
    """Validate every JSONL line against the schema. Returns True if all pass."""
    from jsonschema import Draft202012Validator

    validator = Draft202012Validator(schema)
    failures = 0
    total = 0
    examples: list[str] = []

    for path in files:
        with path.open() as f:
            for lineno, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    continue
                total += 1
                errs = list(validator.iter_errors(obj))
                if errs:
                    failures += 1
                    if len(examples) < 10:
                        top = errs[0]
                        loc = " → ".join(str(p) for p in top.absolute_path) or "(root)"
                        examples.append(
                            f"  {path.name}:{lineno} type={obj.get('type', '?')} "
                            f"[{loc}]: {top.message[:120]}"
                        )

    if failures:
        print(f"\nValidation: {failures}/{total} events failed", file=sys.stderr)
        for ex in examples:
            print(ex, file=sys.stderr)
        if failures > len(examples):
            print(f"  ... and {failures - len(examples)} more", file=sys.stderr)
        return False
    else:
        print(f"\nValidation: all {total} events pass", file=sys.stderr)
        return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dir",
        type=Path,
        default=Path.home() / ".claude" / "projects",
        help="Root directory to scan for .jsonl files (default: ~/.claude/projects)",
    )
    parser.add_argument(
        "-o", "--output",
        type=Path,
        default=None,
        help="Output file (default: stdout)",
    )
    parser.add_argument(
        "--validate",
        action="store_true",
        help="Validate all JSONL events against the generated schema",
    )
    args = parser.parse_args()

    files = find_jsonl_files(args.dir)
    if not files:
        print(f"No .jsonl files found under {args.dir}", file=sys.stderr)
        sys.exit(1)

    result = scan_files(files)
    schema = assemble_root_schema(result.type_schemas)
    apply_enums(schema, result.enum_values)
    apply_sub_discriminators(schema, result)

    if args.validate:
        ok = validate_schema(schema, files)
        if not ok:
            sys.exit(1)

    output = json.dumps(schema, indent=2) + "\n"
    if args.output:
        args.output.write_text(output)
        print(f"\nWrote schema to {args.output}", file=sys.stderr)
    else:
        print(output)


if __name__ == "__main__":
    main()
