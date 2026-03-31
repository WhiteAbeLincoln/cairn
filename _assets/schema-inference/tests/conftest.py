import json
from pathlib import Path

import pytest

FIXTURES_DIR = Path(__file__).parent / "fixtures"


@pytest.fixture
def pet_store_schema() -> dict:
    return json.loads((FIXTURES_DIR / "pet_store.json").read_text())


@pytest.fixture
def messaging_schema() -> dict:
    return json.loads((FIXTURES_DIR / "messaging.json").read_text())


@pytest.fixture
def k8s_workload_schema() -> dict:
    return json.loads((FIXTURES_DIR / "k8s_workload.json").read_text())
