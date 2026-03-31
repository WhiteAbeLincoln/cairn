"""CLI for schema-inference: detect enum/discriminator fields from JSONL files.

    uv run --project _assets/schema-inference -m schema_inference [--dir DIR] [--max-value-length N]
"""

import argparse
import json
import sys
from pathlib import Path

from .detect import DetectConfig, detect_enum_fields


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


def main():
    parser = argparse.ArgumentParser(description="Detect enum/discriminator fields from JSONL files")
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
    args = parser.parse_args()

    objects = load_jsonl_objects(args.dir)
    if not objects:
        print(f"No JSONL objects found under {args.dir}", file=sys.stderr)
        sys.exit(1)

    config = DetectConfig(
        max_value_length=args.max_value_length,
        max_unique_values=args.max_unique_values,
        min_observations=args.min_observations,
        min_score=args.min_score,
    )

    fields = detect_enum_fields(objects, config)

    print(f"\nDetected {len(fields)} enum/discriminator fields:\n", file=sys.stderr)
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

    # JSON output to stdout
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


if __name__ == "__main__":
    main()
