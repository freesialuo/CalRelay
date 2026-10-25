"""Routing must save work without treating stale or failed code as verified."""
from copy import deepcopy
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

import yaml

from scripts.ci_plan import changed_paths, gate, image_relevant, plan, relevant
from scripts.image_release import PREFIX, REPOSITORY, PolicyError

HEAD = 'a' * 40
BASE = 'b' * 40
MERGE = 'c' * 40
EVENT = {'action': 'edited', 'pull_request': {'head': {'sha': HEAD, 'ref': 'ci/123-test',
          'repo': {'full_name': REPOSITORY}}, 'base': {'sha': BASE}}}


class API:
    def __init__(self):
        self.run = {'id': 10, 'workflow_id': 42, 'run_number': 10, 'run_attempt': 1,
                    'head_sha': HEAD, 'head_branch': 'ci/123-test', 'event': 'pull_request',
                    'repository': {'full_name': REPOSITORY}, 'head_repository': {'full_name': REPOSITORY},
                    'status': 'completed', 'conclusion': 'success'}
        self.runs = [self.run]
        self.fresh = self.run
        self.jobs = [{'name': name, 'status': 'completed', 'conclusion': 'success'}
                     for name in (f'checks ({MERGE})', 'rust', 'frontend', 'quality-gate')]

    def get(self, path):
        if path == PREFIX + '/actions/runs/20':
            return {'workflow_id': 42, 'path': '.github/workflows/ci.yml'}
        if '/actions/runs?' in path:
            return deepcopy({'total_count': len(self.runs), 'workflow_runs': self.runs})
        if '/jobs?' in path:
            return deepcopy({'total_count': len(self.jobs), 'jobs': self.jobs})
        if path == PREFIX + '/actions/runs/10':
            return deepcopy(self.fresh)
        raise PolicyError('Unknown fixture endpoint')


class RoutingTests(unittest.TestCase):
    def test_docs_and_python_stay_fast_but_unknown_or_shared_inputs_run_all(self):
        self.assertEqual(relevant(['docs/architecture.md', 'tests/test_pr.py']), {'rust': False, 'frontend': False})
        self.assertEqual(relevant(['web/tests/admin.spec.ts']), {'rust': False, 'frontend': True})
        for paths in (None, ['unknown.new'], ['Cargo.lock'], ['crates/x/src/example.md'],
                      ['web/src/main.ts'], ['schemas/event-v1.schema.json'], ['scripts/ci_plan.py']):
            with self.subTest(paths=paths):
                self.assertEqual(relevant(paths), {'rust': True, 'frontend': True})

    def test_unreadable_diff_is_not_empty_and_renames_include_both_paths(self):
        with patch('scripts.ci_plan.subprocess.run', side_effect=subprocess.CalledProcessError(1, 'git')):
            paths = changed_paths(EVENT)
            self.assertIsNone(paths)
            self.assertEqual(relevant(paths), {'rust': True, 'frontend': True})
        with patch('scripts.ci_plan.subprocess.run', return_value=subprocess.CompletedProcess('git', 0, b'docs/a.md\0Cargo.toml\0')) as command:
            self.assertEqual(changed_paths(EVENT), ['docs/a.md', 'Cargo.toml'])
            self.assertIn('--no-renames', command.call_args.args[0])

    def test_main_and_manual_are_full_even_with_docs_only_paths(self):
        for event in ('push', 'workflow_dispatch'):
            route = plan(event, {}, ['README.md'])
            self.assertTrue(route['rust'])
            self.assertTrue(route['frontend'])
            self.assertEqual(route['proof'], 'full')

    def test_metadata_reuses_exact_tested_merge_head_repo_workflow(self):
        route = plan('pull_request', EVENT, ['Cargo.lock'], API(), 20, MERGE)
        self.assertEqual((route['rust'], route['frontend'], route['proof']), (False, False, 'receipt'))
        self.assertEqual(route['receipt_id'], 10)
        gate(route, 'success', {'rust': 'skipped', 'frontend': 'skipped'}, {'rust': True, 'frontend': True}, {'id': 10, 'attempt': 1})

    def test_latest_failed_or_pending_run_cannot_reuse_older_green(self):
        for status, conclusion in (('completed', 'failure'), ('completed', 'cancelled'), ('in_progress', None)):
            api = API()
            api.runs = [api.run, {**api.run, 'id': 11, 'run_number': 11, 'status': status, 'conclusion': conclusion}]
            route = plan('pull_request', EVENT, ['Cargo.lock'], api, 20, MERGE)
            self.assertEqual((route['rust'], route['frontend'], route['proof']), (True, True, 'full'))

    def test_mismatched_base_head_repo_workflow_attempt_or_jobs_falls_back(self):
        for fault in ('merge', 'head', 'repo', 'fork', 'workflow', 'attempt', 'job', 'missing'):
            api = API()
            if fault == 'merge': api.jobs[0]['name'] = 'checks (' + BASE + ')'
            if fault == 'head': api.run['head_sha'] = BASE
            if fault == 'repo': api.run['repository'] = {'full_name': 'other/repo'}
            if fault == 'fork': api.run['head_repository'] = {'full_name': 'fork/repo'}
            if fault == 'workflow': api.run['workflow_id'] = 43
            if fault == 'attempt': api.fresh = {**api.run, 'run_attempt': 2}
            if fault == 'job': api.jobs[1]['conclusion'] = 'failure'
            if fault == 'missing': api.runs = []
            with self.subTest(fault=fault):
                route = plan('pull_request', EVENT, ['Cargo.lock'], api, 20, MERGE)
                self.assertTrue(route['rust'])
                self.assertTrue(route['frontend'])

    def test_gate_rejects_failure_cancelled_missing_or_unjustified_skip(self):
        route = plan('push', {}, [])
        for checks, rust, front in (('failure', 'success', 'success'), ('cancelled', 'success', 'success'),
                                    ('success', 'skipped', 'success'), ('success', 'success', 'cancelled'),
                                    ('success', 'failure', 'success')):
            with self.subTest(checks=checks, rust=rust, front=front), self.assertRaises(PolicyError):
                gate(route, checks, {'rust': rust, 'frontend': front}, {'rust': True, 'frontend': True})
        route = plan('pull_request', EVENT, ['README.md'])
        gate(route, 'success', {'rust': 'skipped', 'frontend': 'skipped'}, {'rust': False, 'frontend': False})
        invalid = {**route, 'proof': 'unknown'}
        with self.assertRaises(PolicyError):
            gate(invalid, 'success', {'rust': 'skipped', 'frontend': 'skipped'}, {'rust': False, 'frontend': False})
        route['rust'] = True
        with self.assertRaises(PolicyError):
            gate(route, 'success', {'rust': 'success', 'frontend': 'skipped'}, {'rust': False, 'frontend': False})

    def test_gate_rechecks_receipt_not_just_saved_plan(self):
        route = plan('pull_request', EVENT, ['Cargo.lock'], API(), 20, MERGE)
        with self.assertRaises(PolicyError):
            gate(route, 'success', {'rust': 'skipped', 'frontend': 'skipped'}, {'rust': True, 'frontend': True}, {'id': 10, 'attempt': 2})

    def test_image_inputs_and_unknown_paths_build_but_non_inputs_skip(self):
        for path in ('Dockerfile', '.dockerignore', 'compose.yaml', 'crates/x/src/lib.rs',
                     'web/src/main.ts', 'web/package.json', 'assets/new.file', '.github/workflows/container-image.yml'):
            with self.subTest(path=path): self.assertTrue(image_relevant([path]))
        self.assertTrue(image_relevant(None))
        for path in ('docs/architecture.md', 'scripts/image_release.py', 'tests/test_pr.py',
                     'crates/x/tests/roundtrip.rs', 'web/tests/admin.spec.ts', '.github/workflows/ci.yml'):
            with self.subTest(path=path): self.assertFalse(image_relevant([path]))

    def test_workflow_keeps_aggregate_metadata_check_and_trusted_cache_boundaries(self):
        root = Path(__file__).resolve().parents[1]
        ci = yaml.load((root / '.github/workflows/ci.yml').read_text(), Loader=yaml.BaseLoader)
        self.assertEqual(ci['on']['pull_request']['types'], ['opened', 'synchronize', 'reopened', 'edited'])
        self.assertIn("'metadata'", ci['concurrency']['group'])
        self.assertEqual(set(ci['jobs']), {'checks', 'rust', 'frontend', 'quality-gate'})
        self.assertEqual(ci['jobs']['quality-gate']['if'], '${{ always() }}')
        self.assertEqual(set(ci['jobs']['quality-gate']['needs']), {'checks', 'rust', 'frontend'})
        self.assertIn('python3 scripts/check_pr.py', ci['jobs']['quality-gate']['steps'][-1]['run'])
        for name in ('rust', 'frontend'):
            cache = next(s for s in ci['jobs'][name]['steps'] if s.get('uses', '').startswith('Swatinem/rust-cache@'))
            self.assertIn("github.ref == 'refs/heads/main'", cache['with']['save-if'])
            self.assertEqual(ci['jobs'][name]['needs'], 'checks')
        image = yaml.load((root / '.github/workflows/container-image.yml').read_text(), Loader=yaml.BaseLoader)
        self.assertNotIn('ready_for_review', image['on']['pull_request']['types'])
        steps = image['jobs']['build']['steps']
        self.assertEqual(steps[1]['run'], 'python3 -m scripts.ci_plan image')
        for step in steps[2:]:
            self.assertIn("steps.route.outputs.run_image == 'true'", step['if'])
