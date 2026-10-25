"""Inspection reports facts and has no publication or settings mutation path."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import yaml

from scripts import package_inspect as inspect
from scripts.image_release import PACKAGE, PREFIX, REPOSITORY, PolicyError
from test_image_release import API, SHA, WORKFLOW_SHA


class PackageInspectionTests(unittest.TestCase):
    def test_allowlisted_metadata_distinguishes_private_public_and_missing_association(self):
        for visibility, repository in (('private', None), ('public', {'full_name': REPOSITORY}),
                                       ('private', {'full_name': REPOSITORY})):
            api = API()
            api.responses[PACKAGE] = {'visibility': visibility, 'repository': repository,
                                      'created_at': '2026-10-25T13:32:00Z', 'unknown_secret': 'DO_NOT_LOG'}
            api.responses[PACKAGE + '/versions?per_page=100&page=1'] = [{
                'name': 'sha256:' + 'd' * 64, 'created_at': '2026-10-25T13:32:01Z',
                'metadata': {'container': {'tags': [inspect.VERSION]}}, 'unknown_secret': 'DO_NOT_LOG'}]
            facts = inspect.metadata_facts(api)
            self.assertEqual(facts['visibility'], visibility)
            self.assertEqual(facts['repository_matches_expected'], repository is not None)
            self.assertEqual(len(facts['matching_versions_first_page']), 1)
            self.assertNotIn('DO_NOT_LOG', json.dumps(facts))
            self.assertEqual(api.calls, [PACKAGE, PACKAGE + '/versions?per_page=100&page=1'])

    def test_package_404_is_explicitly_unknown_or_absent(self):
        facts = inspect.metadata_facts(API())
        self.assertEqual(facts['package_lookup'], '404_unknown_or_absent')
        self.assertNotIn('visibility', facts)

    def test_registry_config_compares_exact_tested_image_without_running_it(self):
        class Registry:
            def version_digest(self, version):
                self.version = version
                return 'sha256:' + 'd' * 64
        registry = Registry()
        refs = []

        def manifest(ref):
            refs.append(ref)
            return {'config': {'digest': inspect.EXPECTED_IMAGE}}

        facts = inspect.registry_facts(registry, manifest)
        self.assertTrue(facts['matches_run_40197637059_tested_image'])
        self.assertEqual(registry.version, inspect.VERSION)
        self.assertEqual(refs, [inspect.IMAGE + '@sha256:' + 'd' * 64])
        facts = inspect.registry_facts(registry, lambda _: {'config': {'digest': 'sha256:' + 'e' * 64}})
        self.assertFalse(facts['matches_run_40197637059_tested_image'])

    def test_inspection_requires_current_main_control_and_original_tag(self):
        api = API()
        from test_image_release import ImageReleasePolicyTests
        policy = ImageReleasePolicyTests()
        policy.api = api
        context = {k: v for k, v in policy.request().items() if k != 'api'}
        context['ref'] = inspect.REF
        api.responses[PREFIX + '/git/ref/' + inspect.REF[5:]] = {
            'ref': inspect.REF, 'object': {'type': 'commit', 'sha': WORKFLOW_SHA}}
        api.responses[PREFIX + '/git/ref/tags/' + inspect.VERSION] = {
            'ref': 'refs/tags/' + inspect.VERSION, 'object': {'type': 'commit', 'sha': SHA}}
        with patch.object(inspect, 'SOURCE', SHA):
            inspect.qualify_inspection(api, context)
        with self.assertRaises(PolicyError):
            inspect.qualify_inspection(api, context)  # Real fixed alpha SHA differs.
        for changes in ({'event_name': 'pull_request'}, {'ref': 'refs/heads/main'},
                        {'workflow_sha': 'e' * 40}):
            with self.subTest(changes=changes), self.assertRaises(PolicyError):
                inspect.qualify_inspection(api, {**context, **changes})

    def test_workflow_inspection_is_read_only_and_cannot_reach_build_publish(self):
        path = Path(__file__).resolve().parents[1] / '.github/workflows/container-image.yml'
        workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
        job = workflow['jobs']['inspect']
        self.assertEqual(job['permissions'], {'contents': 'read', 'actions': 'read', 'packages': 'read'})
        self.assertIn(inspect.REF, job['if'])
        self.assertIn(inspect.REF, workflow['jobs']['build']['if'])
        self.assertNotIn('release-inspect', workflow['jobs']['publish']['if'])
        self.assertFalse(any('build-push-action@' in s.get('uses', '') for s in job['steps']))
        self.assertEqual(job['steps'][-1]['run'], 'python3 -m scripts.package_inspect')
