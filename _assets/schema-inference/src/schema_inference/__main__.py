"""CLI for schema-inference: detect enum fields and discriminators from JSONL files.

    uv run --project _assets/schema-inference -m schema_inference enums [--dir DIR]
    uv run --project _assets/schema-inference -m schema_inference discriminators [--dir DIR]
"""

import argparse
import json
import sys
from pathlib import Path

from .detect import DetectConfig, detect_enum_fields
from .discriminators import score_discriminators
from .schema import infer_schema


def load_jsonl_objects(root: Path) -> list[dict]:
    objects = []
    errors = 0
    files = sorted(root.rglob("*.jsonl"))
    for path in files:
        with path.open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    objects.append(json.loads(line))
                except json.JSONDecodeError:
                    errors += 1
    print(f"Loaded {len(objects)} objects from {len(files)} files ({errors} parse errors)", file=sys.stderr)
    return objects


def add_common_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--dir",
        type=Path,
        default=Path.home() / ".claude" / "projects",
        help="Root directory to scan for .jsonl files (default: ~/.claude/projects)",
    )
    parser.add_argument("--max-value-length", type=int, default=25)
    parser.add_argument("--max-unique-values", type=int, default=50)
    parser.add_argument("--min-observations", type=int, default=5)
    parser.add_argument("--min-score", type=float, default=0.4)


def make_config(args: argparse.Namespace) -> DetectConfig:
    return DetectConfig(
        max_value_length=args.max_value_length,
        max_unique_values=args.max_unique_values,
        min_observations=args.min_observations,
        min_score=args.min_score,
    )


def cmd_enums(args: argparse.Namespace) -> None:
    objects = load_jsonl_objects(args.dir)
    if not objects:
        print(f"No JSONL objects found under {args.dir}", file=sys.stderr)
        sys.exit(1)

    fields = detect_enum_fields(objects, make_config(args))

    print(f"\nDetected {len(fields)} enum fields:\n", file=sys.stderr)
    for f in fields:
        values = sorted(f.values)
        hint = " (name hint)" if f.is_name_hint else ""
        print(f"  {f.score:.3f}  {f.path}{hint}", file=sys.stderr)
        print(f"         {f.count} observations, {len(values)} unique values", file=sys.stderr)
        if len(values) <= 15:
            print(f"         values: {values}", file=sys.stderr)
        else:
            print(f"         values: {values[:10]} ... +{len(values) - 10} more", file=sys.stderr)
        print(file=sys.stderr)

    output = [
        {
            "path": f.path,
            "values": sorted(f.values),
            "count": f.count,
            "score": f.score,
            "is_name_hint": f.is_name_hint,
        }
        for f in fields
    ]
    print(json.dumps(output, indent=2))


def cmd_discriminators(args: argparse.Namespace) -> None:
    objects = load_jsonl_objects(args.dir)
    if not objects:
        print(f"No JSONL objects found under {args.dir}", file=sys.stderr)
        sys.exit(1)

    config = make_config(args)
    candidates = detect_enum_fields(objects, config)
    print(f"Found {len(candidates)} enum candidates, scoring structural divergence...", file=sys.stderr)

    discs = score_discriminators(objects, candidates, min_score=args.min_divergence)

    print(f"\nDetected {len(discs)} discriminator fields:\n", file=sys.stderr)
    for d in discs:
        values = sorted(d.field.values)
        print(f"  {d.score:.3f}  {d.field.path}", file=sys.stderr)
        print(f"         {d.field.count} observations, {len(values)} values: {values}", file=sys.stderr)
        for val in sorted(d.per_value_keys):
            unique = d.per_value_keys[val] - set().union(
                *(d.per_value_keys[v] for v in d.per_value_keys if v != val)
            )
            shared = d.per_value_keys[val] - unique
            if unique:
                print(f"         {val}: unique={sorted(unique)}, shared={sorted(shared)}", file=sys.stderr)
            else:
                print(f"         {val}: keys={sorted(d.per_value_keys[val])}", file=sys.stderr)
        print(file=sys.stderr)

    output = [
        {
            "path": d.field.path,
            "divergence_score": d.score,
            "enum_score": d.field.score,
            "values": sorted(d.field.values),
            "count": d.field.count,
            "per_value_keys": {v: sorted(ks) for v, ks in sorted(d.per_value_keys.items())},
        }
        for d in discs
    ]
    print(json.dumps(output, indent=2))


def cmd_schema(args: argparse.Namespace) -> None:
    objects = load_jsonl_objects(args.dir)
    if not objects:
        print(f"No JSONL objects found under {args.dir}", file=sys.stderr)
        sys.exit(1)

    config = make_config(args)
    print("Detecting enums and discriminators...", file=sys.stderr)
    schema = infer_schema(
        objects,
        detect_config=config,
        min_divergence=args.min_divergence,
        discriminator_hints=args.discriminator_hints or None,
    )

    if args.validate:
        from jsonschema import Draft202012Validator

        validator = Draft202012Validator(schema)
        failures = 0
        total = len(objects)
        examples: list[str] = []
        for i, obj in enumerate(objects):
            errs = list(validator.iter_errors(obj))
            if errs:
                failures += 1
                if len(examples) < 10:
                    top = errs[0]
                    loc = " -> ".join(str(p) for p in top.absolute_path) or "(root)"
                    examples.append(f"  object {i} type={obj.get('type', '?')} [{loc}]: {top.message[:120]}")
        if failures:
            print(f"\nValidation: {failures}/{total} objects failed", file=sys.stderr)
            for ex in examples:
                print(ex, file=sys.stderr)
            if failures > len(examples):
                print(f"  ... and {failures - len(examples)} more", file=sys.stderr)
        else:
            print(f"\nValidation: all {total} objects pass", file=sys.stderr)

    # Report schema structure
    if "oneOf" in schema:
        disc = schema.get("discriminator", {}).get("propertyName", "?")
        n = len(schema["oneOf"])
        consts = [
            v.get("properties", {}).get(disc, {}).get("const", "?")
            for v in schema["oneOf"]
        ]
        print(f"\nSchema: oneOf with {n} variants, discriminator={disc}", file=sys.stderr)
        print(f"  variants: {consts}", file=sys.stderr)
    else:
        print(f"\nSchema: flat object (no discriminator detected)", file=sys.stderr)

    output = json.dumps(schema, indent=2) + "\n"
    if args.output:
        args.output.write_text(output)
        print(f"Wrote schema to {args.output}", file=sys.stderr)
    else:
        print(output)


def main():
    parser = argparse.ArgumentParser(
        description="Detect enum fields and structural discriminators from JSONL files",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    enum_parser = subparsers.add_parser("enums", help="Detect likely enum fields")
    add_common_args(enum_parser)

    disc_parser = subparsers.add_parser("discriminators", help="Detect structural discriminators")
    add_common_args(disc_parser)
    disc_parser.add_argument(
        "--min-divergence", type=float, default=0.1,
        help="Minimum structural divergence score (default: 0.1)",
    )

    schema_parser = subparsers.add_parser("schema", help="Generate JSON Schema with auto-detected discriminators")
    add_common_args(schema_parser)
    schema_parser.add_argument(
        "--min-divergence", type=float, default=0.1,
        help="Minimum structural divergence score (default: 0.1)",
    )
    schema_parser.add_argument(
        "-o", "--output", type=Path, default=None,
        help="Output file (default: stdout)",
    )
    schema_parser.add_argument(
        "--validate", action="store_true",
        help="Validate all input objects against the generated schema",
    )
    schema_parser.add_argument(
        "--discriminator", action="append", default=[], dest="discriminator_hints",
        help="Manually specify a field as a discriminator (can be repeated)",
    )

    args = parser.parse_args()

    if args.command == "enums":
        cmd_enums(args)
    elif args.command == "discriminators":
        cmd_discriminators(args)
    elif args.command == "schema":
        cmd_schema(args)


if __name__ == "__main__":
    main()
