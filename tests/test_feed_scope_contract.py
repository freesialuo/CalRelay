"""Finite feed policy shape: no wildcard/expression grants or defaulted v1 arrays."""
import json
from pathlib import Path
import unittest
from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[1]

class FeedScopeContract(unittest.TestCase):
    def test_explicit_finite_policy_and_legacy_compatibility(self):
        schema = json.loads((ROOT / 'schemas/feed-scope-v1.schema.json').read_text())
        Draft202012Validator.check_schema(schema)
        validator = Draft202012Validator(schema)
        valid = [{}, {'version': 1, 'calendar_ids': [], 'visible_fields': []},
                 {'version': 1, 'calendar_ids': ['c'], 'visible_fields': ['summary', 'url']}]
        for value in valid:
            with self.subTest(value=value):
                self.assertFalse(list(validator.iter_errors(value)))
        invalid = [[], None, {'scope': '*'}, {'version': 1, 'calendar_ids': []},
                   {'version': 2, 'calendar_ids': [], 'visible_fields': []},
                   {'version': 1, 'calendar_ids': ['c', 'c'], 'visible_fields': []},
                   {'version': 1, 'calendar_ids': [], 'visible_fields': ['organizer']},
                   {'version': 1, 'calendar_ids': [], 'visible_fields': ['url', 'url']},
                   {'version': 1, 'calendar_ids': [], 'visible_fields': [], 'filter': 'true'}]
        for value in invalid:
            with self.subTest(value=value):
                self.assertTrue(list(validator.iter_errors(value)))
