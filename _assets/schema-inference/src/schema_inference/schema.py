"""Generate a JSON Schema from objects using auto-detected discriminators.

Pipeline:
1. Detect enum candidates
2. Score discriminators (structural divergence)
3. Pick the best top-level discriminator
4. Partition objects by discriminator value
5. Infer per-partition schemas with genson
6. Apply enum constraints scoped per partition
7. Assemble into oneOf with discriminator annotation
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any

from genson import SchemaBuilder

from .detect import DetectConfig, EnumField, detect_enum_fields
from .discriminators import Discriminator, score_discriminators, _resolve_parent


def _pick_top_discriminator(
    discriminators: list[Discriminator],
) -> Discriminator | None:
    """Pick the best top-level discriminator.

    Prefers: shallowest path (fewest dots), then highest score.
    """
    if not discriminators:
        return None
    # Filter to root-level fields (no * in path — not inside arrays)
    root_discs = [d for d in discriminators if "*" not in d.field.path]
    if not root_discs:
        return None
    # Sort by depth (ascending), then score (descending)
    root_discs.sort(key=lambda d: (d.field.path.count("."), -d.score))
    return root_discs[0]


def _partition_objects(
    objects: list[dict[str, Any]],
    disc: Discriminator,
) -> dict[str, list[dict[str, Any]]]:
    """Partition objects by a discriminator field's value."""
    path_parts = disc.field.path.split(".")
    leaf = path_parts[-1]
    partitions: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for obj in objects:
        parents = _resolve_parent(obj, path_parts)
        matched = False
        for parent in parents:
            val = parent.get(leaf)
            # Normalize booleans to their string representation
            if isinstance(val, bool):
                val = str(val).lower()
            if isinstance(val, str) and val in disc.field.values:
                partitions[val].append(obj)
                matched = True
                break  # each object counted once
        # For boolean discriminators, absent field → false partition
        if not matched and disc.field.is_boolean and "false" in disc.field.values:
            partitions["false"].append(obj)
    return dict(partitions)


def _infer_base_schema(objects: list[dict[str, Any]]) -> dict:
    """Use genson to infer a merged schema from objects."""
    builder = SchemaBuilder()
    for obj in objects:
        builder.add_object(obj)
    schema = builder.to_schema()
    # Remove genson's $schema since we set our own
    schema.pop("$schema", None)
    return schema


def _collect_enum_values_for_path(
    objects: list[dict[str, Any]],
    path: str,
) -> set[str]:
    """Collect all string values at a dotpath across objects."""
    parts = path.split(".")
    values: set[str] = set()

    def _walk(obj: Any, remaining: list[str]) -> None:
        if not remaining:
            if isinstance(obj, str):
                values.add(obj)
            return
        key, *rest = remaining
        if key == "*":
            if isinstance(obj, list):
                for item in obj:
                    _walk(item, rest)
        elif isinstance(obj, dict) and key in obj:
            _walk(obj[key], rest)

    for obj in objects:
        _walk(obj, parts)
    return values


def _apply_enum_to_schema(schema: dict, path_parts: list[str], values: list[str]) -> None:
    """Walk into a schema node and replace the leaf with an enum."""
    if not path_parts:
        return
    key, *rest = path_parts

    if key == "*":
        if items := schema.get("items"):
            _apply_enum_to_schema(items, rest, values)
        # Also check oneOf in items
        for variant in schema.get("items", {}).get("oneOf", []):
            _apply_enum_to_schema(variant, rest, values)
        return

    props = schema.get("properties", {})
    if key not in props:
        return

    if not rest:
        existing = props[key]
        existing_type = existing.get("type") if isinstance(existing, dict) else None
        # Preserve nullable: if genson inferred ["string", "null"], keep null
        if isinstance(existing_type, list) and "null" in existing_type:
            props[key] = {"type": ["string", "null"], "enum": [*values, None]}
        else:
            props[key] = {"type": "string", "enum": values}
    else:
        _apply_enum_to_schema(props[key], rest, values)


def _apply_enums(
    schema: dict,
    objects: list[dict[str, Any]],
    enum_fields: list[EnumField],
    disc_path: str | None,
) -> None:
    """Apply enum constraints to a schema, scoped to the given objects."""
    for ef in enum_fields:
        # Skip the discriminator field itself (it gets const)
        if ef.path == disc_path:
            continue
        # Boolean fields are already typed correctly by genson; don't override
        if ef.is_boolean:
            continue
        # Collect values scoped to this partition's objects
        values = _collect_enum_values_for_path(objects, ef.path)
        if values and (len(values) >= 2 or len(ef.values) == 1):
            _apply_enum_to_schema(schema, ef.path.split("."), sorted(values))


def _pin_discriminator(
    schema: dict, disc_path: str, disc_val: str, is_boolean: bool,
) -> None:
    """Pin a discriminator field to a const value in a variant schema."""
    leaf = disc_path.split(".")[-1]
    const_val: Any = disc_val
    if is_boolean:
        const_val = disc_val == "true"
        schema.setdefault("properties", {})[leaf] = {"const": const_val}
        if const_val is True:
            if "required" in schema and leaf not in schema["required"]:
                schema["required"].append(leaf)
        # false variant: don't add to required — field can be absent
    else:
        schema.setdefault("properties", {})[leaf] = {"const": const_val}
        if "required" in schema and leaf not in schema["required"]:
            schema["required"].append(leaf)


def _collect_at_path(
    objects: list[dict[str, Any]], path_parts: list[str],
) -> list[dict[str, Any]]:
    """Collect all dicts reachable by following path_parts through objects."""
    results: list[dict[str, Any]] = []
    for obj in objects:
        _walk_to(obj, path_parts, results)
    return results


def _walk_to(
    obj: Any, parts: list[str], results: list[dict[str, Any]],
) -> None:
    if not parts:
        if isinstance(obj, dict):
            results.append(obj)
        return
    key, *rest = parts
    if key == "*":
        if isinstance(obj, list):
            for item in obj:
                _walk_to(item, rest, results)
    elif isinstance(obj, dict) and key in obj:
        _walk_to(obj[key], rest, results)


def _schema_location(
    schema: dict, parent_parts: list[str],
) -> tuple[dict | None, str | None]:
    """Find (container, key) in the schema tree so container[key] is the node to replace."""
    node = schema
    for i, part in enumerate(parent_parts):
        if i == len(parent_parts) - 1:
            # Last segment — this is where to replace
            if part == "*":
                # Direct items, or inside an anyOf array branch
                if "items" in node:
                    return node, "items"
                for branch in node.get("anyOf", []):
                    if branch.get("type") == "array" and "items" in branch:
                        return branch, "items"
                return None, None
            else:
                return node.get("properties", {}), part
        # Navigate deeper
        if part == "*":
            if "items" in node:
                node = node["items"]
            else:
                # Check anyOf for array branch
                found = False
                for branch in node.get("anyOf", []):
                    if branch.get("type") == "array" and "items" in branch:
                        node = branch["items"]
                        found = True
                        break
                if not found:
                    return None, None
        else:
            node = node.get("properties", {}).get(part, {})
    return None, None


def _apply_nested_discriminators(
    schema: dict,
    objects: list[dict[str, Any]],
    discriminators: list[Discriminator],
    top_disc_path: str,
    min_score: float = 0.5,
) -> None:
    """Apply all non-top-level discriminators as nested oneOf in the schema."""
    for disc in discriminators:
        if disc.field.path == top_disc_path:
            continue

        path_parts = disc.field.path.split(".")
        # Root-level fields are handled by top-level partitioning / hints
        if len(path_parts) < 2:
            continue

        # Require strong structural divergence for nested splits
        if disc.score < min_score:
            continue

        leaf = path_parts[-1]
        parent_parts = path_parts[:-1]

        # Collect sub-objects from data at the discriminator's parent level
        sub_objects = _collect_at_path(objects, parent_parts)
        if len(sub_objects) < 2:
            continue

        # Partition sub-objects by discriminator value
        partitions: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for sub_obj in sub_objects:
            val = sub_obj.get(leaf)
            if isinstance(val, bool):
                val = str(val).lower()
            if isinstance(val, str) and val in disc.field.values:
                partitions[val].append(sub_obj)

        if len(partitions) < 2:
            continue

        # Build per-value schemas
        sub_variants = []
        for val in sorted(partitions):
            variant = _infer_base_schema(partitions[val])
            _pin_discriminator(variant, leaf, val, disc.field.is_boolean)
            sub_variants.append(variant)

        # Navigate schema tree and replace the node with oneOf
        container, key = _schema_location(schema, parent_parts)
        if container is not None and key is not None and key in container:
            container[key] = {
                "oneOf": sub_variants,
                "discriminator": {"propertyName": leaf},
            }


def _make_hint_discriminator(
    objects: list[dict[str, Any]],
    path: str,
) -> Discriminator | None:
    """Synthesize a Discriminator from a user-provided hint path.

    Scans objects to discover actual values at the path. For boolean fields,
    uses "true"/"false" as values. Returns None if the field isn't found.
    """
    parts = path.split(".")
    leaf = parts[-1]
    values: set[str] = set()
    is_boolean = False
    count = 0

    for obj in objects:
        parents = _resolve_parent(obj, parts)
        for parent in parents:
            val = parent.get(leaf)
            if isinstance(val, bool):
                values.add(str(val).lower())
                is_boolean = True
                count += 1
            elif isinstance(val, str):
                values.add(val)
                count += 1

    if not values:
        return None

    # For booleans, ensure both true and false are represented
    if is_boolean:
        values = {"true", "false"}

    ef = EnumField(
        path=path,
        values=frozenset(values),
        count=count,
        score=1.0,
        is_name_hint=False,
        is_boolean=is_boolean,
    )

    # Build per_value_keys for metadata (not strictly needed but keeps the type complete)
    per_value_keys: dict[str, set[str]] = defaultdict(set)
    for obj in objects:
        parents = _resolve_parent(obj, parts)
        for parent in parents:
            val = parent.get(leaf)
            if isinstance(val, bool):
                val = str(val).lower()
            if isinstance(val, str) and val in values:
                per_value_keys[val] |= {k for k in parent if k != leaf}

    return Discriminator(field=ef, score=1.0, per_value_keys=dict(per_value_keys))


def infer_schema(
    objects: list[dict[str, Any]],
    detect_config: DetectConfig | None = None,
    min_divergence: float = 0.1,
    discriminator_hints: list[str] | None = None,
) -> dict:
    """Infer a JSON Schema from objects, auto-detecting discriminators and enums.

    Args:
        discriminator_hints: Dotpaths to fields that should be treated as
            discriminators regardless of auto-detection thresholds.

    Returns a JSON Schema with oneOf + discriminator if a structural discriminator
    is found, otherwise a plain merged schema.
    """
    if detect_config is None:
        detect_config = DetectConfig()

    # Step 1-2: detect enums and discriminators
    enum_fields = detect_enum_fields(objects, detect_config)
    discriminators = score_discriminators(objects, enum_fields, min_score=min_divergence)

    # Step 3: pick top-level discriminator
    top_disc = _pick_top_discriminator(discriminators)

    if top_disc is None:
        # No discriminator found — check if a hint applies directly
        if discriminator_hints:
            for hint_path in discriminator_hints:
                hint_disc = _make_hint_discriminator(objects, hint_path)
                if hint_disc is not None:
                    top_disc = hint_disc
                    break
        if top_disc is None:
            schema = _infer_base_schema(objects)
            schema["$schema"] = "https://json-schema.org/draft/2020-12/schema"
            _apply_enums(schema, objects, enum_fields, None)
            return schema

    # Step 4: partition objects
    disc_field = top_disc.field.path
    partitions = _partition_objects(objects, top_disc)

    # Step 5-6: per-partition schema + enums
    # Remove used hint so it's not re-applied in sub-partitions
    remaining_hints = [h for h in (discriminator_hints or []) if h != disc_field]

    variants = []
    for disc_val in sorted(partitions):
        partition_objects = partitions[disc_val]

        # Check if any hint applies as a sub-discriminator within this partition
        sub_disc = None
        if remaining_hints:
            for hint_path in remaining_hints:
                sub_disc = _make_hint_discriminator(partition_objects, hint_path)
                if sub_disc is not None:
                    break

        if sub_disc is not None:
            # Sub-partition by the hint discriminator
            sub_partitions = _partition_objects(partition_objects, sub_disc)
            sub_variants = []
            sub_leaf = sub_disc.field.path.split(".")[-1]
            for sub_val in sorted(sub_partitions):
                sub_objects = sub_partitions[sub_val]
                sv = _infer_base_schema(sub_objects)
                _pin_discriminator(sv, disc_field, disc_val, top_disc.field.is_boolean)
                _pin_discriminator(sv, sub_disc.field.path, sub_val, sub_disc.field.is_boolean)
                _apply_enums(sv, sub_objects, enum_fields, disc_field)
                _apply_nested_discriminators(sv, sub_objects, discriminators, disc_field)
                sub_variants.append(sv)
            variants.extend(sub_variants)
        else:
            variant_schema = _infer_base_schema(partition_objects)
            _pin_discriminator(variant_schema, disc_field, disc_val, top_disc.field.is_boolean)
            _apply_enums(variant_schema, partition_objects, enum_fields, disc_field)
            _apply_nested_discriminators(variant_schema, partition_objects, discriminators, disc_field)
            variants.append(variant_schema)

    # Step 7: assemble
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "oneOf": variants,
        "discriminator": {"propertyName": disc_field.split(".")[-1]},
    }
