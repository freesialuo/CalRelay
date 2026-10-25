from copy import deepcopy
import json
from pathlib import Path
import unittest
from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parents[1]


class SourceContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.config_schema = json.loads((ROOT / 'schemas/source-configuration-v1.schema.json').read_text())
        cls.config = json.loads((ROOT / 'examples/source-configuration-v1.json').read_text())
        cls.envelope_schema = json.loads((ROOT / 'schemas/standard-calendar-source-v1.schema.json').read_text())
        event_schema = json.loads((ROOT / 'schemas/event-v1.schema.json').read_text())
        registry = Registry().with_resource(event_schema['$id'], Resource.from_contents(event_schema))
        for schema in [cls.config_schema, cls.envelope_schema]:
            Draft202012Validator.check_schema(schema)
        cls.configuration = Draft202012Validator(cls.config_schema, format_checker=FormatChecker())
        cls.envelope = Draft202012Validator(cls.envelope_schema, registry=registry, format_checker=FormatChecker())
        cls.batch = {
            'protocol_version': 'standard-calendar-source/v1', 'batch_id': 'batch-1',
            'mode': 'snapshot', 'base_source_revision': 0, 'base_config_revision': 1,
            'base_checkpoint': None, 'proposed_checkpoint': 'next', 'complete': True,
            'pages': 1, 'collection_tag': None,
            'operations': [{'op': 'upsert', 'calendar_id': 'calendar-a',
                            'event': json.loads((ROOT / 'examples/event-v1.json').read_text())}]
        }

    def test_configuration_example_and_strict_fields(self):
        self.configuration.validate(self.config)
        for field in ['source_id', 'revision', 'raw_token', 'arbitrary_plugin']:
            body = deepcopy(self.config)
            body[field] = 'forged'
            self.assertFalse(self.configuration.is_valid(body), field)
        body = deepcopy(self.config)
        body['capabilities']['execute'] = True
        self.assertFalse(self.configuration.is_valid(body))
        body = deepcopy(self.config)
        body['version'] = 'v2'
        self.assertFalse(self.configuration.is_valid(body))

    def test_envelope_reuses_event_v1_and_rejects_forgery(self):
        self.envelope.validate(self.batch)
        mutations = [
            lambda b: b.update(source_id='other'),
            lambda b: b.update(protocol_version='v2'),
            lambda b: b.update(complete=False),
            lambda b: b.update(pages=33),
            lambda b: b.update(pages=2),
            lambda b: b.update(base_checkpoint='not-null'),
            lambda b: b['operations'][0].update(op='execute'),
            lambda b: b['operations'][0]['event'].update(source_id='other'),
            lambda b: b['operations'][0]['event'].update(schema_version='2.0'),
        ]
        for mutate in mutations:
            body = deepcopy(self.batch)
            mutate(body)
            self.assertFalse(self.envelope.is_valid(body), body)
        body = deepcopy(self.batch)
        body.update(pages=2, collection_tag='same-collection')
        self.envelope.validate(body)

    def test_delta_delete_is_explicit_and_conditional(self):
        body = deepcopy(self.batch)
        body.update(mode='delta', base_checkpoint='old')
        body['operations'] = [{'op': 'delete', 'external_uid': 'uid', 'expected_event_revision': 1}]
        self.envelope.validate(body)
        for revision in [None, 0, -1]:
            body['operations'][0]['expected_event_revision'] = revision
            self.assertFalse(self.envelope.is_valid(body))
        body['operations'][0]['expected_event_revision'] = 1
        body['mode'] = 'snapshot'
        self.assertFalse(self.envelope.is_valid(body))


class ReminderConfigurationContractTests(unittest.TestCase):
    def test_existing_rule_policy_and_bark_reference_examples_match_schema(self):
        for name in ['rule', 'bark-output']:
            schema = json.loads((ROOT / f'schemas/{name}-configuration-v1.schema.json').read_text())
            example = json.loads((ROOT / f'examples/{name}-configuration-v1.json').read_text())
            Draft202012Validator.check_schema(schema)
            validator = Draft202012Validator(schema, format_checker=FormatChecker())
            validator.validate(example)
            body = deepcopy(example)
            body['version'] = 'v2'
            self.assertFalse(validator.is_valid(body))
            body = deepcopy(example)
            body['device_key'] = 'synthetic-never-a-real-token'
            self.assertFalse(validator.is_valid(body))

    def test_rule_binding_and_budget_shapes_reject_ambiguous_configuration(self):
        rule = json.loads((ROOT / 'examples/rule-configuration-v1.json').read_text())
        validator = Draft202012Validator(json.loads((ROOT / 'schemas/rule-configuration-v1.schema.json').read_text()))
        for mutate in [
            lambda r: r['policy'].update(calendar_ids=[]),
            lambda r: r['policy'].update(calendar_ids=['calendar-a', 'calendar-a']),
            lambda r: r['policy'].update(lead_ms=604800001),
            lambda r: r['policy'].update(all_day_time='24:00'),
            lambda r: r['policy'].update(unknown=True),
            lambda r: r.update(output_ids=[]),
        ]:
            body = deepcopy(rule)
            mutate(body)
            self.assertFalse(validator.is_valid(body))
