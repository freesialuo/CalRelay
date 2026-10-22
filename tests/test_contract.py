from copy import deepcopy
import json
from pathlib import Path
import unittest

from jsonschema import Draft202012Validator, FormatChecker


ROOT = Path(__file__).resolve().parents[1]


class EventContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.schema = json.loads((ROOT / 'schemas/event-v1.schema.json').read_text())
        cls.example = json.loads((ROOT / 'examples/event-v1.json').read_text())
        Draft202012Validator.check_schema(cls.schema)
        cls.validator = Draft202012Validator(cls.schema, format_checker=FormatChecker())

    def test_documented_example_is_valid(self):
        self.validator.validate(self.example)

    def test_time_variants_preserve_distinct_shapes(self):
        for start in [
            {'kind': 'date', 'date': '2026-11-02'},
            {'kind': 'utc', 'value': '2026-11-02T17:00:00Z'},
            {'kind': 'floating', 'local': '2026-11-02T09:00:00'},
        ]:
            with self.subTest(start=start):
                event = deepcopy(self.example)
                event['start'] = start
                event.pop('recurrence')
                self.validator.validate(event)

    def test_ambiguous_or_forged_documents_are_rejected(self):
        mutations = {
            'end and duration': lambda e: e.update(end=e['start']),
            'missing timezone': lambda e: e['start'].pop('tzid'),
            'empty timezone': lambda e: e['start'].update(tzid=''),
            'forged source': lambda e: e.update(source_id='another-source'),
            'forged revision': lambda e: e.update(revision=999),
            'unsupported version': lambda e: e.update(schema_version='2.0'),
            'invalid calendar date': lambda e: e.update(start={'kind': 'date', 'date': '2026-02-30'}),
            'UTC without offset': lambda e: e.update(start={'kind': 'utc', 'value': '2026-11-02T09:00:00'}),
            'invalid UTC calendar date': lambda e: e.update(start={'kind': 'utc', 'value': '2026-02-30T09:00:00Z'}),
            'unpaired repeat': lambda e: e['alarms'][0].update(repeat=2),
            'missing display text': lambda e: e['alarms'][0].pop('description'),
            'invalid duration': lambda e: e.update(duration='P'),
        }
        for label, mutate in mutations.items():
            with self.subTest(case=label):
                event = deepcopy(self.example)
                mutate(event)
                self.assertTrue(list(self.validator.iter_errors(event)), label)

    def test_cancelled_exception_does_not_require_new_start(self):
        event = deepcopy(self.example)
        event['recurrence']['exceptions'][0]['change'] = {'kind': 'cancelled'}
        self.validator.validate(event)

    def test_rdate_period_requires_one_end_representation(self):
        event = deepcopy(self.example)
        period = {'kind': 'period', 'start': deepcopy(event['start']), 'duration': 'PT1H'}
        event['recurrence']['rdates'] = [period]
        self.validator.validate(event)
        period['end'] = deepcopy(event['start'])
        self.assertTrue(list(self.validator.iter_errors(event)))

    def test_repeat_needs_interval_and_display_text(self):
        event = deepcopy(self.example)
        event['alarms'][0].update(repeat=2, repeat_interval='PT5M')
        self.validator.validate(event)


if __name__ == '__main__':
    unittest.main()
