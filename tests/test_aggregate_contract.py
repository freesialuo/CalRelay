"""Finite aggregate policy structure; cross-resource and total budgets live in Rust."""
import copy
import json
from pathlib import Path
import unittest
from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[1]

class AggregateContract(unittest.TestCase):
    def test_explicit_selection_and_unknown_rules_fail_closed(self):
        schema = json.loads((ROOT / 'schemas/aggregate-subscription-v1.schema.json').read_text())
        Draft202012Validator.check_schema(schema)
        validator = Draft202012Validator(schema)
        example = json.loads((ROOT / 'examples/aggregate-subscription-v1.json').read_text())
        self.assertFalse(list(validator.iter_errors(example)))
        empty = copy.deepcopy(example)
        empty['sources'] = []
        empty['visible_fields'] = []
        self.assertFalse(list(validator.iter_errors(empty)))
        for key in example:
            missing = copy.deepcopy(example)
            del missing[key]
            with self.subTest(missing=key):
                self.assertTrue(list(validator.iter_errors(missing)))
        for key, value in [('version', 2), ('version', 'v1'), ('sources', [{}]),
                           ('visible_fields', ['organizer']), ('visible_fields', ['url', 'url']),
                           ('stale_policy', 'omit'), ('expires_at', -1), ('filter', '*')]:
            bad = copy.deepcopy(example)
            bad[key] = value
            with self.subTest(key=key, value=value):
                self.assertTrue(list(validator.iter_errors(bad)))
