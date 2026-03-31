"""Detect which enum fields are structural discriminators.

Given a list of enum candidates (from detect.py), partitions objects by each
candidate's values and measures how much the sibling property sets diverge
across partitions. Fields whose values correlate with structural differences
score high; fields with identical structure across values score low.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any

from .detect import EnumField


@dataclass(frozen=True)
class Discriminator:
    """An enum field that structurally discriminates sibling properties."""

    field: EnumField
    """The underlying enum field."""

    score: float
    """Divergence score (0-1). Higher = stronger discriminator."""

    per_value_keys: dict[str, set[str]]
    """For each discriminator value, the set of property keys observed at the
    same level as the discriminator field."""


def _resolve_parent(obj: Any, path_parts: list[str]) -> list[dict]:
    """Walk obj and return the parent dict(s) containing the leaf field."""
    if len(path_parts) == 1:
        if isinstance(obj, dict) and path_parts[0] in obj:
            return [obj]
        return []
    key, *rest = path_parts
    if key == "*":
        if isinstance(obj, list):
            out = []
            for item in obj:
                out.extend(_resolve_parent(item, rest))
            return out
        return []
    if isinstance(obj, dict) and key in obj:
        return _resolve_parent(obj[key], rest)
    return []


def _jaccard_distance(a: set, b: set) -> float:
    """Jaccard distance between two sets. 0 = identical, 1 = disjoint."""
    if not a and not b:
        return 0.0
    union = a | b
    intersection = a & b
    return 1.0 - len(intersection) / len(union)


def _compute_divergence(per_value_keys: dict[str, set[str]]) -> float:
    """Compute structural divergence score from per-value key sets.

    Measures the average pairwise Jaccard distance between property key sets
    for each discriminator value. Ignores the discriminator field itself.

    Returns 0.0 for identical structures, 1.0 for completely disjoint.
    """
    values = list(per_value_keys.keys())
    if len(values) < 2:
        return 0.0

    # Compute pairwise Jaccard distances
    total_dist = 0.0
    n_pairs = 0
    for i in range(len(values)):
        for j in range(i + 1, len(values)):
            total_dist += _jaccard_distance(
                per_value_keys[values[i]],
                per_value_keys[values[j]],
            )
            n_pairs += 1

    return total_dist / n_pairs if n_pairs > 0 else 0.0


def score_discriminators(
    objects: list[dict[str, Any]],
    candidates: list[EnumField],
    min_score: float = 0.1,
) -> list[Discriminator]:
    """Score enum candidates on how strongly they discriminate sibling structure.

    For each candidate, partitions objects by the candidate's values, collects
    the set of property keys at the same level for each partition, and scores
    based on how different those key sets are.

    Returns discriminators sorted by score (highest first), filtered by min_score.
    """
    results = []

    for candidate in candidates:
        # Single-value enums can't discriminate
        if len(candidate.values) < 2:
            continue

        path_parts = candidate.path.split(".")
        leaf_name = path_parts[-1]

        # Partition: for each object, find the parent dict, get the
        # discriminator value and the sibling keys
        per_value_keys: dict[str, set[str]] = defaultdict(set)
        per_value_count: dict[str, int] = defaultdict(int)

        for obj in objects:
            parents = _resolve_parent(obj, path_parts)
            for parent in parents:
                val = parent.get(leaf_name)
                if not isinstance(val, str) or val not in candidate.values:
                    continue
                # Collect sibling keys (excluding the discriminator itself)
                sibling_keys = {k for k in parent.keys() if k != leaf_name}
                per_value_keys[val] |= sibling_keys
                per_value_count[val] += 1

        # Need at least 2 values actually observed with siblings
        observed = {v for v, c in per_value_count.items() if c > 0}
        if len(observed) < 2:
            continue

        divergence = _compute_divergence(dict(per_value_keys))

        if divergence >= min_score:
            results.append(Discriminator(
                field=candidate,
                score=round(divergence, 3),
                per_value_keys=dict(per_value_keys),
            ))

    results.sort(key=lambda d: (-d.score, d.field.path))
    return results
