#!/usr/bin/env python3
"""Validate spec/openapi/*.json against the OpenAPI 3.1 schema.

    pip install openapi-spec-validator
    python3 scripts/validate_openapi.py
"""

import glob
import sys

from openapi_spec_validator import validate
from openapi_spec_validator.readers import read_from_filename

failed = False
for path in sorted(glob.glob("spec/openapi/*.json")):
    doc, _ = read_from_filename(path)
    try:
        validate(doc)
        ops = sum(1 for item in doc["paths"].values() for k in item if not k.startswith("x-"))
        print(f"{path}: valid OpenAPI {doc['openapi']}, {len(doc['paths'])} paths, {ops} operations")
    except Exception as e:  # noqa: BLE001
        failed = True
        print(f"{path}: INVALID\n{str(e)[:2000]}")
sys.exit(1 if failed else 0)
