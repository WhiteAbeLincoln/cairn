"""Auto-detect enum and discriminator fields from a collection of JSON objects.

Walks all property paths in the objects and scores each path on how likely it
is to be an enum or discriminator field, based on configurable heuristics:

- Value type: only string-valued fields are considered
- Value length: individual values must be short (default <=25 chars)
- Cardinality: the number of unique values must be small relative to total count
- Name hints: property names like "type", "kind", "status" get a bonus
- Entropy: values should look like identifiers, not random strings
"""

from __future__ import annotations

import math
import re
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from typing import Any


# Property names that strongly suggest a discriminator or enum field.
DEFAULT_NAME_HINTS: set[str] = {
    "type", "kind", "status", "state", "role", "mode", "category",
    "action", "event", "operation", "method", "level", "severity",
    "variant", "subtype", "userType", "eventType",
}

# Regex for "identifier-like" strings: lowercase/uppercase letters, digits,
# hyphens, underscores, dots, colons. No spaces or special chars.
_IDENTIFIER_RE = re.compile(r"^[a-zA-Z][a-zA-Z0-9_.\-:/]*$")


@dataclass(frozen=True)
class EnumField:
    """A detected enum/discriminator field."""

    path: str
    """Dotpath to the field, e.g. "message.content.*.type"."""

    values: frozenset[str]
    """The set of distinct string values observed."""

    count: int
    """Total number of observations (including repeats)."""

    score: float
    """Confidence score (0-1). Higher = more likely an enum."""

    is_name_hint: bool
    """Whether the leaf property name matched a name hint."""


@dataclass
class DetectConfig:
    """Tunable parameters for enum/discriminator detection."""

    max_value_length: int = 25
    """Maximum string length for a value to be considered an enum member."""

    max_unique_values: int = 50
    """Maximum number of distinct values before a field is excluded."""

    min_observations: int = 5
    """Minimum total observations needed to consider a field."""

    min_score: float = 0.4
    """Minimum score for a field to be included in results."""

    name_hints: set[str] = field(default_factory=lambda: DEFAULT_NAME_HINTS.copy())
    """Property names that get a score bonus."""

    name_hint_bonus: float = 0.2
    """Score bonus for matching a name hint."""


@dataclass
class _FieldStats:
    """Accumulator for a single dotpath during scanning."""

    values: Counter[str] = field(default_factory=Counter)
    total: int = 0
    non_string: int = 0


def _walk_paths(obj: Any, prefix: str, stats: dict[str, _FieldStats]) -> None:
    """Recursively walk a JSON object, recording string values at each dotpath."""
    if isinstance(obj, dict):
        for key, val in obj.items():
            path = f"{prefix}.{key}" if prefix else key
            if isinstance(val, str):
                s = stats[path]
                s.values[val] += 1
                s.total += 1
            elif isinstance(val, (int, float, bool)) or val is None:
                stats[path].non_string += 1
                stats[path].total += 1
            elif isinstance(val, dict):
                _walk_paths(val, path, stats)
            elif isinstance(val, list):
                _walk_array(val, path, stats)
    # scalars at top level are ignored


def _walk_array(arr: list, prefix: str, stats: dict[str, _FieldStats]) -> None:
    """Walk array items, using * as the array element placeholder."""
    array_path = f"{prefix}.*"
    for item in arr:
        if isinstance(item, str):
            s = stats[array_path]
            s.values[item] += 1
            s.total += 1
        elif isinstance(item, dict):
            _walk_paths(item, f"{prefix}.*", stats)
        elif isinstance(item, list):
            _walk_array(item, array_path, stats)


def _identifier_ratio(values: Counter[str]) -> float:
    """Fraction of distinct values that look like identifiers."""
    if not values:
        return 0.0
    id_count = sum(1 for v in values if _IDENTIFIER_RE.match(v))
    return id_count / len(values)


def _entropy_score(values: Counter[str]) -> float:
    """Normalized entropy of value distribution (0=uniform noise, 1=concentrated).

    We want fields where some values dominate (low entropy relative to max).
    Returns 1.0 for single-value fields, lower for more spread-out distributions.
    """
    total = sum(values.values())
    if total == 0 or len(values) <= 1:
        return 1.0
    max_entropy = math.log2(len(values))
    if max_entropy == 0:
        return 1.0
    entropy = -sum((c / total) * math.log2(c / total) for c in values.values())
    # Invert: low entropy (concentrated) -> high score
    return 1.0 - (entropy / max_entropy)


def _score_field(
    path: str, stats: _FieldStats, config: DetectConfig,
) -> EnumField | None:
    """Score a single field path and return an EnumField if it passes thresholds."""
    # Must have enough observations
    if stats.total < config.min_observations:
        return None

    # Must be string-only (allow a small fraction of non-string for messy data)
    string_count = sum(stats.values.values())
    if string_count == 0:
        return None
    if stats.non_string > string_count * 0.1:
        return None

    unique = stats.values
    n_unique = len(unique)

    # Too many unique values
    if n_unique > config.max_unique_values:
        return None

    # All values must be short
    if any(len(v) > config.max_value_length for v in unique):
        return None

    # Compute component scores (each 0-1)

    # Cardinality: fewer unique values -> higher score
    # 1 value = 1.0, max_unique_values = 0.0
    cardinality_score = 1.0 - (n_unique - 1) / max(config.max_unique_values - 1, 1)

    # Identifier-likeness
    id_ratio = _identifier_ratio(unique)

    # Entropy (concentration)
    entropy = _entropy_score(unique)

    # Weighted combination
    score = (
        0.35 * cardinality_score
        + 0.35 * id_ratio
        + 0.30 * entropy
    )

    # Name hint bonus
    leaf_name = path.rsplit(".", 1)[-1] if "." in path else path
    is_hint = leaf_name in config.name_hints
    if is_hint:
        score = min(1.0, score + config.name_hint_bonus)

    if score < config.min_score:
        return None

    return EnumField(
        path=path,
        values=frozenset(unique),
        count=string_count,
        score=round(score, 3),
        is_name_hint=is_hint,
    )


def _collapse_map_keys(
    stats: dict[str, _FieldStats], max_siblings: int = 20,
) -> dict[str, _FieldStats]:
    """Detect map-like objects (many unique keys under one parent) and remove them.

    For each prefix in all paths, counts how many distinct next-segment values
    appear. If a prefix has more than max_siblings distinct next-segments,
    it's likely a map (additionalProperties) and all paths through it are dropped.
    """
    # For every possible prefix, collect the distinct next path segment
    children_by_prefix: dict[str, set[str]] = defaultdict(set)
    for path in stats:
        parts = path.split(".")
        for i in range(len(parts) - 1):
            prefix = ".".join(parts[: i + 1])
            next_segment = parts[i + 1]
            children_by_prefix[prefix].add(next_segment)

    # Find prefixes with too many children (map-like)
    map_prefixes = {
        prefix for prefix, children in children_by_prefix.items()
        if len(children) > max_siblings
    }

    if not map_prefixes:
        return stats

    # Filter out all paths that pass through a map prefix
    return {
        path: s for path, s in stats.items()
        if not any(
            path.startswith(prefix + ".") for prefix in map_prefixes
        )
    }


def detect_enum_fields(
    objects: list[dict[str, Any]],
    config: DetectConfig | None = None,
) -> list[EnumField]:
    """Detect likely enum/discriminator fields from a list of JSON objects.

    Returns fields sorted by score (highest first).
    """
    if config is None:
        config = DetectConfig()

    stats: dict[str, _FieldStats] = defaultdict(_FieldStats)
    for obj in objects:
        _walk_paths(obj, "", stats)

    stats = _collapse_map_keys(stats)

    results = []
    for path, field_stats in stats.items():
        ef = _score_field(path, field_stats, config)
        if ef is not None:
            results.append(ef)

    results.sort(key=lambda f: (-f.score, f.path))
    return results
