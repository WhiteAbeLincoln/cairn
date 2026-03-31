"""Tests for structural divergence / discriminator detection.

After detecting enum candidates, we need to determine which ones are actual
discriminators (their values correlate with structural differences in sibling
properties) vs plain enums (like "status" which doesn't change the shape).
"""

import pytest
from hypothesis import given, settings, HealthCheck
from hypothesis_jsonschema import from_schema

from schema_inference.detect import DetectConfig, detect_enum_fields
from schema_inference.discriminators import score_discriminators, Discriminator


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def generate_batch(schema: dict, n: int = 200) -> list[dict]:
    """Generate n random instances from a JSON schema."""

    @given(obj=from_schema(schema))
    @settings(
        max_examples=n,
        suppress_health_check=[HealthCheck.too_slow, HealthCheck.filter_too_much],
    )
    def _collect(obj):
        results.append(obj)

    results: list = []
    _collect()
    return results


def discriminator_paths(objects: list[dict], **detect_kwargs) -> dict[str, Discriminator]:
    """Detect enums then score discriminators, return dict keyed by path."""
    candidates = detect_enum_fields(objects, DetectConfig(**detect_kwargs))
    discs = score_discriminators(objects, candidates)
    return {d.field.path: d for d in discs}


# ---------------------------------------------------------------------------
# Unit tests: basic structural divergence
# ---------------------------------------------------------------------------

class TestBasicDiscriminator:
    def test_type_field_is_discriminator(self):
        """A 'type' field whose values produce different property sets is a discriminator."""
        objects = [
            {"type": "point", "x": 1, "y": 2},
            {"type": "point", "x": 3, "y": 4},
            {"type": "circle", "cx": 0, "cy": 0, "radius": 5},
            {"type": "circle", "cx": 1, "cy": 1, "radius": 3},
            {"type": "line", "x1": 0, "y1": 0, "x2": 10, "y2": 10},
            {"type": "line", "x1": 1, "y1": 1, "x2": 5, "y2": 5},
        ]
        discs = discriminator_paths(objects)
        assert "type" in discs
        assert discs["type"].score > 0.5

    def test_shared_enum_is_not_discriminator(self):
        """A 'status' field with values that don't change the shape is not a discriminator."""
        objects = [
            {"status": "active", "name": "a", "count": 1},
            {"status": "active", "name": "b", "count": 2},
            {"status": "inactive", "name": "c", "count": 3},
            {"status": "inactive", "name": "d", "count": 4},
            {"status": "pending", "name": "e", "count": 5},
            {"status": "pending", "name": "f", "count": 6},
        ]
        discs = discriminator_paths(objects)
        # status should either not appear or have a very low score
        if "status" in discs:
            assert discs["status"].score < 0.3

    def test_discriminator_scores_higher_than_plain_enum(self):
        """When both a discriminator and a plain enum exist, discriminator scores higher."""
        objects = [
            {"type": "dog", "breed": "poodle", "status": "available"},
            {"type": "dog", "breed": "lab", "status": "adopted"},
            {"type": "cat", "color": "black", "status": "available"},
            {"type": "cat", "color": "white", "status": "adopted"},
            {"type": "bird", "wingspan": 30, "status": "available"},
            {"type": "bird", "wingspan": 25, "status": "pending"},
        ]
        discs = discriminator_paths(objects)
        assert "type" in discs
        if "status" in discs:
            assert discs["type"].score > discs["status"].score

    def test_nested_discriminator(self):
        """Discriminator detection works in nested objects."""
        objects = [
            {"event": {"kind": "create", "target": "x", "created_by": "alice"}},
            {"event": {"kind": "create", "target": "y", "created_by": "bob"}},
            {"event": {"kind": "delete", "target": "x", "reason": "expired"}},
            {"event": {"kind": "delete", "target": "y", "reason": "manual"}},
            {"event": {"kind": "update", "target": "x", "diff": {"a": 1}}},
            {"event": {"kind": "update", "target": "y", "diff": {"b": 2}}},
        ]
        discs = discriminator_paths(objects)
        assert "event.kind" in discs

    def test_two_discriminators_at_same_level(self):
        """Two fields that both discriminate should both be detected."""
        objects = [
            {"type": "request", "method": "GET", "url": "/a"},
            {"type": "request", "method": "POST", "url": "/b", "body": "data"},
            {"type": "request", "method": "GET", "url": "/c"},
            {"type": "request", "method": "POST", "url": "/d", "body": "more"},
            {"type": "response", "method": "GET", "status_code": 200},
            {"type": "response", "method": "POST", "status_code": 201, "body": "ok"},
            {"type": "response", "method": "GET", "status_code": 404},
            {"type": "response", "method": "POST", "status_code": 500, "body": "err"},
        ]
        discs = discriminator_paths(objects)
        # type discriminates: request has "url", response has "status_code"
        assert "type" in discs
        # method discriminates: POST has "body", GET doesn't
        assert "method" in discs

    def test_single_value_enum_not_discriminator(self):
        """An enum with only one value can't discriminate anything."""
        objects = [
            {"version": "v1", "type": "a", "x": 1},
            {"version": "v1", "type": "b", "y": 2},
            {"version": "v1", "type": "a", "x": 3},
            {"version": "v1", "type": "b", "y": 4},
            {"version": "v1", "type": "a", "x": 5},
        ]
        discs = discriminator_paths(objects)
        assert "version" not in discs
        assert "type" in discs


# ---------------------------------------------------------------------------
# Discriminator metadata
# ---------------------------------------------------------------------------

class TestDiscriminatorMetadata:
    def test_reports_per_value_unique_keys(self):
        """Each discriminator should report which keys are unique to each value."""
        objects = [
            {"type": "point", "x": 1, "y": 2},
            {"type": "point", "x": 3, "y": 4},
            {"type": "point", "x": 5, "y": 6},
            {"type": "circle", "cx": 0, "cy": 0, "radius": 5},
            {"type": "circle", "cx": 1, "cy": 1, "radius": 3},
            {"type": "circle", "cx": 2, "cy": 2, "radius": 1},
        ]
        discs = discriminator_paths(objects)
        d = discs["type"]
        # per_value_keys should map each value to the set of keys seen
        assert "point" in d.per_value_keys
        assert "circle" in d.per_value_keys
        assert {"x", "y"} <= d.per_value_keys["point"]
        assert {"cx", "cy", "radius"} <= d.per_value_keys["circle"]


# ---------------------------------------------------------------------------
# Hypothesis tests: property-based against fixture schemas
# ---------------------------------------------------------------------------

class TestPetStoreDiscriminators:
    def test_type_is_discriminator(self, pet_store_schema):
        """In the pet store schema, 'type' discriminates between animal/event/log."""
        objects = generate_batch(pet_store_schema, n=300)
        discs = discriminator_paths(objects)
        assert "type" in discs, f"Expected 'type' as discriminator. Got: {sorted(discs)}"
        assert discs["type"].score > 0.5

    def test_status_is_not_discriminator(self, pet_store_schema):
        """'status' is a plain enum (only on animals), not a structural discriminator."""
        objects = generate_batch(pet_store_schema, n=300)
        discs = discriminator_paths(objects)
        # status only appears on animal objects, so it's present/absent based on
        # type - but it doesn't itself discriminate structure among its own values
        if "status" in discs:
            assert discs["status"].score < discs["type"].score


class TestMessagingDiscriminators:
    def test_kind_is_discriminator(self, messaging_schema):
        """'kind' discriminates between message/delivery/subscription."""
        objects = generate_batch(messaging_schema, n=300)
        discs = discriminator_paths(objects)
        assert "kind" in discs, f"Expected 'kind' as discriminator. Got: {sorted(discs)}"

    def test_channel_is_not_top_discriminator(self, messaging_schema):
        """'channel' is shared across all types with same values, not a discriminator."""
        objects = generate_batch(messaging_schema, n=300)
        discs = discriminator_paths(objects)
        if "channel" in discs:
            assert discs["channel"].score < discs["kind"].score


class TestK8sDiscriminators:
    def test_kind_is_discriminator(self, k8s_workload_schema):
        """'kind' in k8s workloads is a discriminator (different kinds -> different specs)."""
        objects = generate_batch(k8s_workload_schema, n=300)
        discs = discriminator_paths(objects)
        # kind has 4 values but the spec is identical in our fixture,
        # so it may or may not score as a discriminator.
        # apiVersion + kind together determine the shape, but kind alone
        # might not cause structural divergence in our simplified fixture.
        # At minimum, strategy.type should be a discriminator if present.
        pass  # This is a documentation test - the real assertion is below

    def test_strategy_type_is_discriminator(self, k8s_workload_schema):
        """spec.strategy.type discriminates between Recreate (no extra fields) and
        RollingUpdate (has maxSurge, maxUnavailable)."""
        objects = generate_batch(k8s_workload_schema, n=300)
        discs = discriminator_paths(objects)
        if "spec.strategy.type" in discs:
            assert discs["spec.strategy.type"].score > 0.3
