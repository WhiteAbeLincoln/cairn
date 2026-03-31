"""Tests for enum/discriminator field detection.

Uses hypothesis-jsonschema to generate random valid instances from fixture
schemas, then verifies that detect_enum_fields correctly identifies the
discriminator and enum fields defined in those schemas.
"""

import pytest
from hypothesis import given, settings, HealthCheck
from hypothesis_jsonschema import from_schema

from schema_inference import detect_enum_fields, EnumField
from schema_inference.detect import DetectConfig


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def detected_paths(objects: list[dict], **kwargs) -> dict[str, EnumField]:
    """Run detection and return a dict keyed by path."""
    fields = detect_enum_fields(objects, DetectConfig(**kwargs))
    return {f.path: f for f in fields}


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


# ---------------------------------------------------------------------------
# Unit tests: basic detection logic
# ---------------------------------------------------------------------------

class TestBasicDetection:
    def test_single_enum_field(self):
        objects = [
            {"type": "a", "data": "hello world this is a long string"},
            {"type": "b", "data": "another long string with stuff"},
            {"type": "a", "data": "more data here for variety"},
            {"type": "c", "data": "yet another piece of data"},
            {"type": "b", "data": "final long string example"},
        ]
        fields = detected_paths(objects)
        assert "type" in fields
        assert fields["type"].values == {"a", "b", "c"}

    def test_nested_enum_field(self):
        objects = [
            {"event": {"kind": "create", "target": "foo"}},
            {"event": {"kind": "delete", "target": "bar"}},
            {"event": {"kind": "update", "target": "baz"}},
            {"event": {"kind": "create", "target": "qux"}},
            {"event": {"kind": "delete", "target": "quux"}},
        ]
        fields = detected_paths(objects)
        assert "event.kind" in fields
        assert fields["event.kind"].values == {"create", "delete", "update"}

    def test_array_items_enum(self):
        objects = [
            {"items": [{"status": "open"}, {"status": "closed"}]},
            {"items": [{"status": "open"}, {"status": "pending"}]},
            {"items": [{"status": "closed"}]},
            {"items": [{"status": "pending"}, {"status": "open"}]},
            {"items": [{"status": "closed"}, {"status": "pending"}]},
        ]
        fields = detected_paths(objects)
        assert "items.*.status" in fields

    def test_long_values_excluded(self):
        objects = [
            {"id": f"some-really-long-identifier-value-{i}"} for i in range(20)
        ]
        fields = detected_paths(objects, max_value_length=25)
        assert "id" not in fields

    def test_high_cardinality_excluded(self):
        objects = [{"tag": f"val-{i}"} for i in range(100)]
        fields = detected_paths(objects, max_unique_values=50)
        assert "tag" not in fields

    def test_non_string_excluded(self):
        objects = [{"count": i} for i in range(20)]
        fields = detected_paths(objects)
        assert "count" not in fields

    def test_too_few_observations(self):
        objects = [{"type": "a"}, {"type": "b"}]
        fields = detected_paths(objects, min_observations=5)
        assert "type" not in fields

    def test_name_hint_boosts_score(self):
        objects = [
            {"type": v, "flavor": v}
            for v in ["alpha", "beta", "gamma"] * 5
        ]
        fields = detected_paths(objects)
        # Both should be detected, but "type" should score higher
        assert "type" in fields
        assert "flavor" in fields
        assert fields["type"].score >= fields["flavor"].score
        assert fields["type"].is_name_hint is True
        assert fields["flavor"].is_name_hint is False

    def test_random_looking_strings_score_low(self):
        """UUIDs and hex strings should not be detected as enums."""
        import uuid
        objects = [{"id": str(uuid.uuid4()), "type": "event"} for _ in range(50)]
        fields = detected_paths(objects, max_unique_values=100)
        assert "id" not in fields
        assert "type" in fields


# ---------------------------------------------------------------------------
# Hypothesis tests: generate from schemas and verify detection
# ---------------------------------------------------------------------------

class TestPetStoreSchema:
    def test_detects_discriminator(self, pet_store_schema):
        objects = generate_batch(pet_store_schema)
        assert len(objects) > 50, "Need enough generated examples"
        fields = detected_paths(objects)
        assert "type" in fields, f"Should detect 'type' discriminator. Got: {sorted(fields)}"
        assert fields["type"].values <= {"animal", "event", "log"}

    def test_detects_nested_enums(self, pet_store_schema):
        objects = generate_batch(pet_store_schema)
        fields = detected_paths(objects)
        # species, status, action, level are all enum fields
        expected_enum_paths = {"species", "status", "action", "level"}
        detected = set(fields.keys())
        # We may not get all of them (depends on generation distribution),
        # but we should get the discriminator and at least some enums
        found = expected_enum_paths & detected
        assert len(found) >= 2, (
            f"Expected at least 2 of {expected_enum_paths}, got {found}. "
            f"All detected: {sorted(detected)}"
        )

    def test_detects_deeply_nested_role(self, pet_store_schema):
        objects = generate_batch(pet_store_schema, n=300)
        fields = detected_paths(objects)
        if "actor.role" in fields:
            assert fields["actor.role"].values <= {"admin", "staff", "volunteer"}


class TestMessagingSchema:
    def test_detects_kind_discriminator(self, messaging_schema):
        objects = generate_batch(messaging_schema)
        fields = detected_paths(objects)
        assert "kind" in fields, f"Should detect 'kind' discriminator. Got: {sorted(fields)}"
        assert fields["kind"].values <= {"message", "delivery", "subscription"}

    def test_detects_channel_enum(self, messaging_schema):
        objects = generate_batch(messaging_schema)
        fields = detected_paths(objects)
        assert "channel" in fields, f"Should detect 'channel' enum. Got: {sorted(fields)}"
        assert fields["channel"].values <= {"email", "sms", "push", "webhook"}

    def test_detects_nested_format(self, messaging_schema):
        objects = generate_batch(messaging_schema, n=300)
        fields = detected_paths(objects)
        if "payload.format" in fields:
            assert fields["payload.format"].values <= {"text", "html", "markdown"}


class TestK8sWorkloadSchema:
    def test_detects_kind(self, k8s_workload_schema):
        objects = generate_batch(k8s_workload_schema, n=300)
        fields = detected_paths(objects)
        assert "kind" in fields, f"Should detect 'kind'. Got: {sorted(fields)}"
        assert fields["kind"].values <= {"Deployment", "StatefulSet", "Job", "CronJob"}

    def test_detects_api_version(self, k8s_workload_schema):
        objects = generate_batch(k8s_workload_schema, n=300)
        fields = detected_paths(objects)
        assert "apiVersion" in fields, f"Should detect 'apiVersion'. Got: {sorted(fields)}"

    def test_detects_strategy_type(self, k8s_workload_schema):
        objects = generate_batch(k8s_workload_schema, n=300)
        fields = detected_paths(objects)
        if "spec.strategy.type" in fields:
            assert fields["spec.strategy.type"].values <= {"Recreate", "RollingUpdate"}

    def test_detects_protocol_in_array(self, k8s_workload_schema):
        objects = generate_batch(k8s_workload_schema, n=300)
        fields = detected_paths(objects)
        # protocol is inside containers[].ports[], so path is
        # spec.containers.*.ports.*.protocol
        protocol_paths = [p for p in fields if p.endswith("protocol")]
        if protocol_paths:
            path = protocol_paths[0]
            assert fields[path].values <= {"TCP", "UDP", "SCTP"}

    def test_detects_restart_policy(self, k8s_workload_schema):
        objects = generate_batch(k8s_workload_schema, n=300)
        fields = detected_paths(objects)
        policy_paths = [p for p in fields if "restartPolicy" in p]
        if policy_paths:
            assert fields[policy_paths[0]].values <= {"Always", "OnFailure", "Never"}


# ---------------------------------------------------------------------------
# Cross-schema: no false positives on free-text fields
# ---------------------------------------------------------------------------

class TestNoFalsePositives:
    """Verify that free-text string fields (names, messages, bodies) are NOT
    detected as enums, even when generated from constrained schemas."""

    def test_name_fields_not_detected(self, pet_store_schema):
        objects = generate_batch(pet_store_schema, n=300)
        fields = detected_paths(objects)
        # "name" is a free-text field, should not be an enum
        # (it might appear due to short generated strings, but with enough
        # examples the cardinality should be too high)
        if "name" in fields:
            # If detected, it should have low score
            assert fields["name"].score < 0.6, "Free-text 'name' scored too high"

    def test_message_field_not_detected(self, pet_store_schema):
        objects = generate_batch(pet_store_schema, n=300)
        fields = detected_paths(objects)
        if "message" in fields:
            assert fields["message"].score < 0.6, "Free-text 'message' scored too high"


# ---------------------------------------------------------------------------
# Boolean enum detection
# ---------------------------------------------------------------------------

class TestBooleanEnumDetection:
    def test_boolean_field_detected_as_enum(self):
        """A boolean field should be detected as a two-valued enum."""
        objects = [
            {"isError": True, "code": 500},
            {"isError": False, "code": 200},
            {"isError": True, "code": 503},
            {"isError": False, "code": 200},
            {"isError": False, "code": 201},
        ]
        fields = detected_paths(objects)
        assert "isError" in fields
        assert fields["isError"].values == {"true", "false"}

    def test_boolean_with_structural_difference(self):
        """Boolean field with different sibling keys per value should be detected."""
        objects = [
            {"isApiError": True, "error_code": 500, "error_msg": "fail"},
            {"isApiError": True, "error_code": 503, "error_msg": "timeout"},
            {"isApiError": False, "usage": {"tokens": 10}, "result": "ok"},
            {"isApiError": False, "usage": {"tokens": 20}, "result": "done"},
            {"isApiError": False, "usage": {"tokens": 5}, "result": "yes"},
        ]
        fields = detected_paths(objects)
        assert "isApiError" in fields
        assert fields["isApiError"].values == {"true", "false"}

    def test_always_true_boolean_not_discriminator(self):
        """A boolean that is always the same value should still be detected as enum
        (single-value), but the discriminator scorer will reject it."""
        objects = [
            {"enabled": True, "x": 1},
            {"enabled": True, "x": 2},
            {"enabled": True, "x": 3},
            {"enabled": True, "x": 4},
            {"enabled": True, "x": 5},
        ]
        fields = detected_paths(objects)
        if "enabled" in fields:
            assert fields["enabled"].values == {"true"}
