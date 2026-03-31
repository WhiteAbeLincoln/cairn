"""Tests for automatic JSON Schema generation from detected discriminators."""

import json

import pytest
from hypothesis import given, settings, HealthCheck
from hypothesis_jsonschema import from_schema
from jsonschema import Draft202012Validator

from schema_inference.schema import infer_schema


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def generate_batch(schema: dict, n: int = 200) -> list[dict]:
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


def validate_all(schema: dict, objects: list[dict]) -> list[str]:
    """Return list of error messages for objects that fail validation."""
    validator = Draft202012Validator(schema)
    errors = []
    for i, obj in enumerate(objects):
        errs = list(validator.iter_errors(obj))
        if errs:
            errors.append(f"object {i}: {errs[0].message[:200]}")
    return errors


# ---------------------------------------------------------------------------
# Unit tests: hand-crafted data
# ---------------------------------------------------------------------------

class TestBasicSchemaGeneration:
    def test_produces_valid_json_schema(self):
        """Output should be a valid JSON Schema document."""
        objects = [
            {"type": "point", "x": 1, "y": 2},
            {"type": "point", "x": 3, "y": 4},
            {"type": "point", "x": 5, "y": 6},
            {"type": "circle", "cx": 0, "cy": 0, "radius": 5},
            {"type": "circle", "cx": 1, "cy": 1, "radius": 3},
            {"type": "circle", "cx": 2, "cy": 2, "radius": 1},
        ]
        schema = infer_schema(objects)
        assert "$schema" in schema
        # Should be parseable by jsonschema
        Draft202012Validator.check_schema(schema)

    def test_discriminated_oneOf(self):
        """Objects with a discriminator should produce oneOf with const tags."""
        objects = [
            {"type": "point", "x": 1, "y": 2},
            {"type": "point", "x": 3, "y": 4},
            {"type": "point", "x": 5, "y": 6},
            {"type": "circle", "cx": 0, "cy": 0, "radius": 5},
            {"type": "circle", "cx": 1, "cy": 1, "radius": 3},
            {"type": "circle", "cx": 2, "cy": 2, "radius": 1},
        ]
        schema = infer_schema(objects)
        assert "oneOf" in schema
        assert "discriminator" in schema
        assert schema["discriminator"]["propertyName"] == "type"

        consts = {
            v["properties"]["type"]["const"]
            for v in schema["oneOf"]
        }
        assert consts == {"point", "circle"}

    def test_all_inputs_validate(self):
        """Every input object should validate against the generated schema."""
        objects = [
            {"type": "point", "x": 1, "y": 2},
            {"type": "point", "x": 3, "y": 4},
            {"type": "point", "x": 5, "y": 6},
            {"type": "circle", "cx": 0, "cy": 0, "radius": 5},
            {"type": "circle", "cx": 1, "cy": 1, "radius": 3},
            {"type": "circle", "cx": 2, "cy": 2, "radius": 1},
        ]
        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors}"

    def test_enum_fields_applied(self):
        """Detected enum fields should have enum constraints in the schema."""
        objects = [
            {"type": "a", "status": "open", "x": 1},
            {"type": "a", "status": "closed", "x": 2},
            {"type": "a", "status": "open", "x": 3},
            {"type": "b", "status": "open", "y": 1},
            {"type": "b", "status": "closed", "y": 2},
            {"type": "b", "status": "pending", "y": 3},
        ]
        schema = infer_schema(objects)
        # Find the "b" variant and check status has enum
        for variant in schema["oneOf"]:
            if variant["properties"]["type"]["const"] == "b":
                status_schema = variant["properties"]["status"]
                assert "enum" in status_schema
                assert set(status_schema["enum"]) == {"open", "closed", "pending"}

    def test_boolean_discriminator_produces_oneOf(self):
        """A boolean discriminator should produce oneOf with const true/false."""
        objects = [
            {"isApiError": True, "error_code": 500, "error_msg": "fail"},
            {"isApiError": True, "error_code": 503, "error_msg": "timeout"},
            {"isApiError": True, "error_code": 400, "error_msg": "bad request"},
            {"isApiError": False, "usage": {"tokens": 10}, "result": "ok"},
            {"isApiError": False, "usage": {"tokens": 20}, "result": "done"},
            {"isApiError": False, "usage": {"tokens": 5}, "result": "yes"},
        ]
        schema = infer_schema(objects)
        assert "oneOf" in schema
        assert schema["discriminator"]["propertyName"] == "isApiError"
        # const values should be actual booleans, not strings
        consts = {
            v["properties"]["isApiError"]["const"]
            for v in schema["oneOf"]
        }
        assert consts == {True, False}

    def test_boolean_discriminator_validates(self):
        """All input objects should validate against schema with boolean discriminator."""
        objects = [
            {"isApiError": True, "error_code": 500, "error_msg": "fail"},
            {"isApiError": True, "error_code": 503, "error_msg": "timeout"},
            {"isApiError": True, "error_code": 400, "error_msg": "bad request"},
            {"isApiError": False, "usage": {"tokens": 10}, "result": "ok"},
            {"isApiError": False, "usage": {"tokens": 20}, "result": "done"},
            {"isApiError": False, "usage": {"tokens": 5}, "result": "yes"},
        ]
        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors}"

    def test_nullable_enum_preserved_after_partitioning(self):
        """When genson infers a nullable type within a partition, enum application
        should preserve nullability instead of overwriting with type: string."""
        objects = [
            # Most objects have string service_tier
            *[{"type": "assistant", "tier": "standard", "data": i} for i in range(20)],
            # A few have null tier (e.g. error responses)
            {"type": "assistant", "tier": None, "data": 100},
            {"type": "assistant", "tier": None, "data": 101},
            # Different type entirely
            *[{"type": "system", "msg": f"m{i}"} for i in range(10)],
        ]
        schema = infer_schema(objects)
        # Find the assistant variant
        for variant in schema["oneOf"]:
            if variant["properties"]["type"]["const"] == "assistant":
                tier = variant["properties"]["tier"]
                # genson should infer ["string", "null"], and enum should preserve null
                assert tier.get("type") == ["string", "null"], f"Expected nullable type, got {tier}"
                assert "enum" in tier
                assert None in tier["enum"], f"Expected None in enum, got {tier['enum']}"
                assert "standard" in tier["enum"]
                break

    def test_manual_discriminator_hint(self):
        """A user-specified discriminator hint should be used even if it's too rare
        to be auto-detected."""
        objects = [
            # 20 normal objects (isApiError absent or false)
            *[{"type": "response", "usage": {"tokens": i}, "result": "ok"} for i in range(15)],
            *[{"type": "response", "isApiError": False, "usage": {"tokens": i}, "result": "ok"} for i in range(3)],
            # 2 error objects (isApiError=True, different shape)
            {"type": "response", "isApiError": True, "error_code": 500},
            {"type": "response", "isApiError": True, "error_code": 503},
        ]
        schema = infer_schema(objects, discriminator_hints=["isApiError"])
        assert "oneOf" in schema
        assert schema["discriminator"]["propertyName"] == "isApiError"

    def test_manual_discriminator_hint_validates(self):
        """All objects should validate against a schema using a manual discriminator hint."""
        objects = [
            *[{"type": "response", "usage": {"tokens": i}, "result": "ok"} for i in range(15)],
            *[{"type": "response", "isApiError": False, "usage": {"tokens": i}, "result": "ok"} for i in range(3)],
            {"type": "response", "isApiError": True, "error_code": 500},
            {"type": "response", "isApiError": True, "error_code": 503},
        ]
        schema = infer_schema(objects, discriminator_hints=["isApiError"])
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors}"

    def test_manual_hint_absent_means_false_for_boolean(self):
        """When a boolean discriminator hint field is absent, it should be treated as false."""
        objects = [
            {"isApiError": True, "error_code": 500},
            {"isApiError": True, "error_code": 503},
            {"isApiError": True, "error_code": 400},
            {"isApiError": False, "usage": {"tokens": 10}},
            # Field absent — should land in the false partition
            {"usage": {"tokens": 20}},
            {"usage": {"tokens": 30}},
            {"usage": {"tokens": 40}},
            {"usage": {"tokens": 50}},
            {"usage": {"tokens": 60}},
        ]
        schema = infer_schema(objects, discriminator_hints=["isApiError"])
        assert "oneOf" in schema
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors}"

    def test_array_nested_discriminator(self):
        """A discriminator inside array items should produce items.oneOf."""
        objects = []
        for i in range(10):
            objects.append({
                "type": "msg",
                "content": [
                    {"kind": "text", "text": f"hello {i}"},
                    {"kind": "code", "lang": "python", "code": f"x = {i}"},
                ],
            })
        for i in range(10):
            objects.append({
                "type": "msg",
                "content": [
                    {"kind": "text", "text": f"world {i}"},
                    {"kind": "image", "url": f"http://img/{i}"},
                ],
            })
        for i in range(5):
            objects.append({"type": "log", "level": "info", "msg": f"log {i}"})

        schema = infer_schema(objects)
        assert "oneOf" in schema
        for v in schema["oneOf"]:
            if v["properties"]["type"]["const"] == "msg":
                items_schema = v["properties"]["content"]["items"]
                assert "oneOf" in items_schema, (
                    f"Expected oneOf in items, got: {list(items_schema.keys())}"
                )
                assert items_schema["discriminator"]["propertyName"] == "kind"
                consts = {
                    sv["properties"]["kind"]["const"]
                    for sv in items_schema["oneOf"]
                }
                assert consts == {"text", "code", "image"}
                break
        else:
            pytest.fail("No 'msg' variant found")

    def test_array_nested_discriminator_validates(self):
        """Objects should validate when array items are discriminated."""
        objects = []
        for i in range(10):
            objects.append({
                "type": "msg",
                "content": [
                    {"kind": "text", "text": f"hello {i}"},
                    {"kind": "code", "lang": "python", "code": f"x = {i}"},
                ],
            })
        for i in range(10):
            objects.append({
                "type": "msg",
                "content": [
                    {"kind": "text", "text": f"world {i}"},
                    {"kind": "image", "url": f"http://img/{i}"},
                ],
            })
        for i in range(5):
            objects.append({"type": "log", "level": "info", "msg": f"log {i}"})

        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors}"

    def test_nested_object_discriminator(self):
        """A discriminator inside a nested object should produce oneOf at that level."""
        objects = [
            *[{"type": "a", "event": {"kind": "create", "target": f"t{i}", "created_by": "x"}} for i in range(5)],
            *[{"type": "a", "event": {"kind": "delete", "target": f"t{i}", "reason": "expired"}} for i in range(5)],
            *[{"type": "a", "event": {"kind": "update", "target": f"t{i}", "diff": {"a": 1}}} for i in range(5)],
            *[{"type": "b", "value": i} for i in range(5)],
        ]
        schema = infer_schema(objects)
        assert "oneOf" in schema
        for v in schema["oneOf"]:
            if v["properties"]["type"]["const"] == "a":
                event_schema = v["properties"]["event"]
                assert "oneOf" in event_schema, (
                    f"Expected oneOf in event, got: {list(event_schema.keys())}"
                )
                assert event_schema["discriminator"]["propertyName"] == "kind"
                break
        else:
            pytest.fail("No 'a' variant found")

    def test_anyof_array_discriminator(self):
        """When genson wraps an array in anyOf (e.g. string | array), the
        discriminator should still be applied inside the array branch's items."""
        objects = []
        # Some objects have content as an array (discriminated items)
        for i in range(10):
            objects.append({
                "type": "msg",
                "content": [
                    {"kind": "text", "text": f"hello {i}"},
                    {"kind": "code", "lang": "py", "code": f"x={i}"},
                ],
            })
        # Some have content as a plain string (triggers genson anyOf)
        for i in range(5):
            objects.append({"type": "msg", "content": f"plain string {i}"})
        # A second type to ensure top-level discrimination
        for i in range(5):
            objects.append({"type": "log", "msg": f"log {i}"})

        schema = infer_schema(objects)
        assert "oneOf" in schema
        for v in schema["oneOf"]:
            if v["properties"]["type"]["const"] == "msg":
                content = v["properties"]["content"]
                # genson should produce anyOf: [string, array]
                assert "anyOf" in content, f"Expected anyOf, got: {list(content.keys())}"
                for branch in content["anyOf"]:
                    if branch.get("type") == "array":
                        items = branch["items"]
                        assert "oneOf" in items, (
                            f"Expected oneOf in array items, got: {list(items.keys())}"
                        )
                        consts = {
                            sv["properties"]["kind"]["const"]
                            for sv in items["oneOf"]
                        }
                        assert consts == {"text", "code"}
                        break
                else:
                    pytest.fail("No array branch in anyOf")
                break
        else:
            pytest.fail("No 'msg' variant found")

    def test_anyof_array_discriminator_validates(self):
        """Objects with anyOf content (string | array) should validate."""
        objects = []
        for i in range(10):
            objects.append({
                "type": "msg",
                "content": [
                    {"kind": "text", "text": f"hello {i}"},
                    {"kind": "code", "lang": "py", "code": f"x={i}"},
                ],
            })
        for i in range(5):
            objects.append({"type": "msg", "content": f"plain string {i}"})
        for i in range(5):
            objects.append({"type": "log", "msg": f"log {i}"})

        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors}"

    def test_no_discriminator_produces_flat_schema(self):
        """When no discriminator is found, produce a plain merged schema."""
        objects = [
            {"name": "alice", "age": 30},
            {"name": "bob", "age": 25},
            {"name": "carol", "age": 35},
            {"name": "dave", "age": 28},
            {"name": "eve", "age": 22},
        ]
        schema = infer_schema(objects)
        # No oneOf since all objects have the same shape
        assert "oneOf" not in schema
        assert schema.get("type") == "object"


# ---------------------------------------------------------------------------
# Property-based tests: generated data round-trips through schema
# ---------------------------------------------------------------------------

class TestPetStoreRoundTrip:
    def test_generated_objects_validate(self, pet_store_schema):
        """Objects generated from pet_store schema should validate against
        the inferred schema."""
        objects = generate_batch(pet_store_schema, n=300)
        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors[:5]}"

    def test_has_discriminator(self, pet_store_schema):
        objects = generate_batch(pet_store_schema, n=300)
        schema = infer_schema(objects)
        assert "oneOf" in schema
        assert "discriminator" in schema


class TestMessagingRoundTrip:
    def test_generated_objects_validate(self, messaging_schema):
        objects = generate_batch(messaging_schema, n=300)
        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors[:5]}"

    def test_has_discriminator(self, messaging_schema):
        objects = generate_batch(messaging_schema, n=300)
        schema = infer_schema(objects)
        assert "oneOf" in schema


class TestK8sRoundTrip:
    def test_generated_objects_validate(self, k8s_workload_schema):
        objects = generate_batch(k8s_workload_schema, n=500)
        schema = infer_schema(objects)
        errors = validate_all(schema, objects)
        assert errors == [], f"Validation errors: {errors[:5]}"
