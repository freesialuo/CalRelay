"""Trust-boundary tests: no publishing, real credentials, or image execution."""
from copy import deepcopy
import unittest

from scripts.image_release import (IMAGE, REPOSITORY, PolicyError, parse_version,
                                  promote_latest, quality_gate, source_plan)

SHA = 'a' * 40
WORKFLOW_SHA = 'c' * 40
DIGEST = 'sha256:' + 'b' * 64
PREFIX = f'repos/{REPOSITORY}'


class API:
    def __init__(self):
        self.responses = {
            PREFIX: {'private': True, 'default_branch': 'main'},
            PREFIX + '/git/ref/tags/v1.2.3': {
                'ref': 'refs/tags/v1.2.3', 'object': {'type': 'commit', 'sha': SHA}},
            PREFIX + f'/compare/{SHA}...main': {
                'status': 'ahead', 'merge_base_commit': {'sha': SHA}},
            PREFIX + '/actions/workflows/ci.yml': {
                'id': 42, 'state': 'active', 'path': '.github/workflows/ci.yml'},
            PREFIX + f'/actions/workflows/42/runs?event=push&branch=main&head_sha={SHA}&per_page=100': {
                'total_count': 1, 'workflow_runs': [self.run()]},
            PREFIX + '/actions/runs/7/jobs?filter=latest&per_page=100': {
                'total_count': 1, 'jobs': [{'name': 'quality-gate', 'status': 'completed',
                                         'conclusion': 'success'}]},
            PREFIX + '/actions/runs/7': self.run(),
            PREFIX + '/releases/9': {'id': 9, 'draft': False, 'prerelease': False,
                                    'tag_name': 'v1.2.3', 'published_at': '2026-10-25T00:00:00Z'},
        }
        self.calls = []

    def run(self, **changes):
        return {'id': 7, 'run_number': 12, 'run_attempt': 1, 'workflow_id': 42,
                'head_sha': SHA, 'head_branch': 'main', 'event': 'push',
                'repository': {'full_name': REPOSITORY},
                'head_repository': {'full_name': REPOSITORY},
                'status': 'completed', 'conclusion': 'success', **changes}

    def get(self, path, missing=False):
        self.calls.append(path)
        value = self.responses.get(path)
        if value is None and not missing:
            raise PolicyError('missing API fixture: ' + path)
        return deepcopy(value)


class ImageReleasePolicyTests(unittest.TestCase):
    def setUp(self):
        self.api = API()
        self.event = {'repository': {'full_name': REPOSITORY}, 'action': 'published',
                      'release': {'id': 9, 'tag_name': 'v1.2.3', 'draft': False,
                                  'prerelease': False}}

    def plan(self, **changes):
        args = dict(event_name='release', event=self.event, ref='refs/tags/v1.2.3',
                    sha=SHA, workflow_sha=WORKFLOW_SHA, api=self.api)
        args.update(changes)
        return source_plan(**args)

    def test_canonical_versions_and_prerelease_rules(self):
        self.assertEqual(parse_version('v0.1.0').core, (0, 1, 0))
        self.assertTrue(parse_version('v1.2.3-rc.1').prerelease)
        for value in ('1.2.3', 'v01.2.3', 'v1.2', 'v1.2.3+metadata', 'v1.2.3-01',
                      'v1.2.3-rc..1', 'v1.2.3\n', 'v1.2.3; echo secret', 'v1.2.3/other', 'v1.2.3٣'):
            with self.subTest(value=value), self.assertRaises(PolicyError):
                parse_version(value)

    def test_release_qualifies_exact_sha_and_current_main_ci(self):
        plan = self.plan()
        self.assertTrue(plan['publish'])
        self.assertEqual(plan['sha'], SHA)
        self.assertEqual(plan['image'], IMAGE)
        self.assertEqual(plan['version'], 'v1.2.3')
        self.assertEqual(plan['ci_run'], 7)

    def test_pr_only_builds_without_registry_or_release_api_access(self):
        event = {'repository': {'full_name': REPOSITORY}, 'number': 71,
                 'pull_request': {'head': {'repo': {'full_name': 'fork/calrelay'}}}}
        plan = self.plan(event_name='pull_request', event=event, ref='refs/pull/71/merge')
        self.assertFalse(plan['publish'])
        self.assertEqual(plan['sha'], SHA)
        self.assertEqual(self.api.calls, [])

    def test_untrusted_events_refs_repositories_and_release_flags_fail_closed(self):
        cases = [dict(event_name='pull_request_target'), dict(event_name='workflow_run'),
                 dict(ref='refs/tags/v9.0.0'), dict(sha='d' * 40),
                 dict(workflow_sha='not-a-sha')]
        for change in cases:
            with self.subTest(change=change), self.assertRaises(PolicyError):
                self.plan(**change)
        for change in ({'repository': {'full_name': 'fork/calrelay'}},
                       {'action': 'edited'},
                       {'release': {**self.event['release'], 'draft': True}},
                       {'release': {**self.event['release'], 'prerelease': True}}):
            with self.subTest(change=change), self.assertRaises(PolicyError):
                self.plan(event={**self.event, **change})

    def test_dispatch_is_main_only_and_defaults_to_no_publish(self):
        event = {'repository': {'full_name': REPOSITORY},
                 'inputs': {'version': 'v1.2.3', 'publish': 'false'}}
        self.assertFalse(self.plan(event_name='workflow_dispatch', event=event,
                                  ref='refs/heads/main')['publish'])
        event['inputs']['publish'] = 'true'
        self.assertTrue(self.plan(event_name='workflow_dispatch', event=event,
                                 ref='refs/heads/main')['publish'])
        with self.assertRaises(PolicyError):
            self.plan(event_name='workflow_dispatch', event=event, ref='refs/heads/feature')
        event['inputs']['publish'] = 'yes'
        with self.assertRaises(PolicyError):
            self.plan(event_name='workflow_dispatch', event=event, ref='refs/heads/main')

    def test_annotated_tag_is_peeled_and_unmerged_source_is_rejected(self):
        path = PREFIX + '/git/ref/tags/v1.2.3'
        self.api.responses[path]['object'] = {'type': 'tag', 'sha': 'e' * 40}
        self.api.responses[PREFIX + '/git/tags/' + 'e' * 40] = {
            'object': {'type': 'commit', 'sha': SHA}}
        self.assertEqual(self.plan()['sha'], SHA)
        self.api.responses[PREFIX + f'/compare/{SHA}...main']['status'] = 'diverged'
        with self.assertRaises(PolicyError):
            self.plan()

    def test_latest_failed_running_or_wrong_identity_cannot_reuse_old_green(self):
        path = PREFIX + f'/actions/workflows/42/runs?event=push&branch=main&head_sha={SHA}&per_page=100'
        for changed in ({'conclusion': 'failure'}, {'status': 'in_progress', 'conclusion': None},
                        {'event': 'pull_request'}, {'head_sha': 'f' * 40},
                        {'workflow_id': 99}, {'head_repository': {'full_name': 'fork/calrelay'}}):
            self.api.responses[path]['workflow_runs'] = [self.api.run(run_number=13, **changed), self.api.run()]
            with self.subTest(changed=changed), self.assertRaises(PolicyError):
                quality_gate(self.api, SHA)

    def test_latest_attempt_and_quality_gate_job_must_remain_successful(self):
        jobs = PREFIX + '/actions/runs/7/jobs?filter=latest&per_page=100'
        self.api.responses[jobs]['jobs'][0]['conclusion'] = 'failure'
        with self.assertRaises(PolicyError):
            quality_gate(self.api, SHA)
        self.api.responses[jobs]['jobs'][0]['conclusion'] = 'success'
        self.api.responses[PREFIX + '/actions/runs/7']['run_attempt'] = 2
        with self.assertRaises(PolicyError):
            quality_gate(self.api, SHA)

    def test_latest_alias_only_moves_forward_for_stable_versions(self):
        current = {'latest': {'digest': DIGEST, 'tags': ['latest', 'v1.2.3']}}
        self.assertFalse(promote_latest(parse_version('v1.2.4-rc.1'), current))
        self.assertFalse(promote_latest(parse_version('v1.2.2'), current))
        self.assertFalse(promote_latest(parse_version('v1.2.3'), current))
        self.assertTrue(promote_latest(parse_version('v1.2.4'), current))
        with self.assertRaises(PolicyError):
            promote_latest(parse_version('v1.2.4'), {'latest': {'digest': DIGEST, 'tags': ['latest']}})




class ImageArtifactTests(unittest.TestCase):
    def setUp(self):
        import tempfile
        from pathlib import Path
        from scripts.image_release import seal_bundle
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.api = API()
        self.context = dict(event_name='release',
                            event={'repository': {'full_name': REPOSITORY}, 'action': 'published',
                                   'release': {'id': 9, 'tag_name': 'v1.2.3', 'draft': False, 'prerelease': False}},
                            ref='refs/tags/v1.2.3', sha=SHA, workflow_sha=WORKFLOW_SHA)
        self.plan = source_plan(api=self.api, **self.context)
        self.record = {'Id': DIGEST, 'Os': 'linux', 'Architecture': 'amd64', 'Config': {'Labels': {
            'org.opencontainers.image.source': 'https://github.com/' + REPOSITORY,
            'org.opencontainers.image.revision': SHA, 'org.opencontainers.image.version': 'v1.2.3'}}}
        (self.directory / 'image.tar').write_bytes(b'synthetic Docker archive for policy tests')
        self.expected = seal_bundle(self.directory, self.plan, self.record, 123, 1)

    def test_bundle_accepts_only_exact_source_run_attempt_and_outputs(self):
        from scripts.image_release import verify_bundle
        self.assertEqual(verify_bundle(self.directory, self.plan, self.expected, 123, 1)['image_id'], DIGEST)
        for plan, run, attempt, expected in (
                ({**self.plan, 'sha': 'd' * 40}, 123, 1, self.expected),
                (self.plan, 124, 1, self.expected), (self.plan, 123, 2, self.expected),
                (self.plan, 123, 1, {**self.expected, 'image_id': 'sha256:' + 'e' * 64})):
            with self.subTest(run=run, attempt=attempt), self.assertRaises(PolicyError):
                verify_bundle(self.directory, plan, expected, run, attempt)

    def test_archive_manifest_extra_files_and_symlinks_cannot_be_substituted(self):
        from scripts.image_release import verify_bundle
        for filename in ('image.tar', 'manifest.json'):
            path = self.directory / filename
            original = path.read_bytes()
            path.write_bytes(original + b'changed')
            with self.subTest(filename=filename), self.assertRaises(PolicyError):
                verify_bundle(self.directory, self.plan, self.expected, 123, 1)
            path.write_bytes(original)
        extra = self.directory / 'script.py'
        extra.write_text('never execute me')
        with self.assertRaises(PolicyError):
            verify_bundle(self.directory, self.plan, self.expected, 123, 1)
        extra.unlink()
        archive = self.directory / 'image.tar'
        saved = archive.read_bytes()
        archive.unlink()
        archive.symlink_to(self.directory / 'manifest.json')
        with self.assertRaises(PolicyError):
            verify_bundle(self.directory, self.plan, self.expected, 123, 1)
        archive.unlink()
        archive.write_bytes(saved)

    def test_wrong_platform_or_labels_fail_before_sealing(self):
        from scripts.image_release import inspect_image
        for changes in ({'Architecture': 'arm64'}, {'Os': 'windows'}, {'Id': 'invalid'},
                        {'Config': {'Labels': {}}}):
            with self.subTest(changes=changes), self.assertRaises(PolicyError):
                inspect_image({**self.record, **changes}, self.plan)

    def test_existing_package_must_remain_private_and_repository_linked(self):
        from scripts.image_release import PACKAGE, package_state
        self.assertEqual(package_state(self.api), {})
        for package in ({'visibility': 'public', 'repository': {'full_name': REPOSITORY}},
                        {'visibility': 'private', 'repository': {'full_name': 'other/repo'}}):
            self.api.responses[PACKAGE] = package
            with self.subTest(package=package), self.assertRaises(PolicyError):
                package_state(self.api)

    def publication(self, *, existing=True, latest=True, failure=None, after_pull=None, live_latest='metadata', metadata_latest=True, live_version='metadata'):
        import json
        from scripts.image_release import PACKAGE, publish_bundle
        registry_digest = 'sha256:' + 'd' * 64
        old_digest = 'sha256:' + 'e' * 64
        tags = ['v1.2.3']
        if latest:
            tags.append('latest')
        versions = [{'name': registry_digest, 'metadata': {'container': {'tags': tags}}}] if existing else []
        if not latest:
            versions.append({'name': old_digest, 'metadata': {'container': {'tags': ['v1.2.2', 'latest']}}})
        if not metadata_latest:
            for entry in versions:
                entry['metadata']['container']['tags'] = [t for t in entry['metadata']['container']['tags'] if t != 'latest']
        self.api.responses[PACKAGE] = {'visibility': 'private', 'repository': {'full_name': REPOSITORY}}
        self.api.responses[PACKAGE + '/versions?per_page=100&page=1'] = versions
        calls = []
        actual_version = (registry_digest if existing else None) if live_version == 'metadata' else live_version
        foreign_digest = 'sha256:' + 'f' * 64

        def docker(*args):
            nonlocal actual_version
            calls.append(args)
            if failure and args[0] == failure:
                raise PolicyError('injected Docker failure')
            if args[:2] == ('image', 'inspect'):
                return json.dumps([self.record])
            if args[:3] == ('buildx', 'imagetools', 'inspect'):
                return json.dumps({'mediaType': 'application/vnd.oci.image.manifest.v1+json',
                                   'config': {'digest': ('sha256:' + 'c' * 64) if args[-1] == IMAGE + '@' + foreign_digest else DIGEST}})
            if args[0] == 'pull' and after_pull:
                after_pull(self.api)
            if args[0] == 'push':
                actual_version = registry_digest
                versions.append({'name': registry_digest, 'metadata': {'container': {'tags': ['v1.2.3']}}})
            if args[:3] == ('buildx', 'imagetools', 'create'):
                (self.directory / 'promotion.json').write_text(json.dumps({
                    'containerimage.descriptor': {'digest': registry_digest}}))
            return ''

        self.docker_calls = calls

        def latest_digest(known_digest):
            self.assertEqual(known_digest, registry_digest)
            if isinstance(live_latest, Exception):
                raise live_latest
            return (registry_digest if latest else old_digest) if live_latest == 'metadata' else live_latest

        def version_digest(version):
            self.assertEqual(version, 'v1.2.3')
            if isinstance(actual_version, Exception):
                raise actual_version
            return actual_version

        result = publish_bundle(self.api, docker, self.context, self.directory,
                                self.expected, 123, 1, lambda _: None, latest_digest, version_digest)
        return result, calls

    def test_idempotent_existing_version_never_repushes_or_downgrades_latest(self):
        result, calls = self.publication()
        self.assertFalse(result['latest_promoted'])
        self.assertFalse(any(c[0] in ('push', 'tag') or c[:3] == ('buildx', 'imagetools', 'create') for c in calls))

    def test_new_version_pushes_tested_image_before_final_stable_alias(self):
        result, calls = self.publication(existing=False, latest=False)
        self.assertTrue(result['latest_promoted'])
        push = next(i for i, c in enumerate(calls) if c[0] == 'push')
        promote = next(i for i, c in enumerate(calls) if c[:3] == ('buildx', 'imagetools', 'create'))
        self.assertLess(push, promote)
        self.assertIn('--prefer-index=false', calls[promote])

    def test_pr_cannot_reach_any_docker_operation_in_publisher(self):
        from scripts.image_release import publish_bundle
        context = {**self.context, 'event_name': 'pull_request', 'ref': 'refs/pull/7/merge',
                   'event': {'repository': {'full_name': REPOSITORY}, 'number': 7}}
        calls = []
        with self.assertRaises(PolicyError):
            publish_bundle(self.api, lambda *args: calls.append(args), context,
                           self.directory, self.expected, 123, 1, lambda _: None, lambda _: None, lambda _: None)
        self.assertEqual(calls, [])

    def test_new_failed_ci_or_moved_tag_blocks_alias_after_image_transfer(self):
        def fail_ci(api):
            path = PREFIX + '/actions/runs/7'
            api.responses[path]['conclusion'] = 'failure'

        def move_tag(api):
            path = PREFIX + '/git/ref/tags/v1.2.3'
            api.responses[path]['object']['sha'] = 'f' * 40

        for change in (fail_ci, move_tag):
            with self.subTest(change=change.__name__), self.assertRaises(PolicyError):
                self.publication(latest=False, after_pull=change)
            self.api = API()

    def test_metadata_lag_does_not_overwrite_existing_different_candidate(self):
        # A previous publication succeeded, but Packages still omits its tag.
        with self.assertRaises(PolicyError):
            self.publication(existing=False, latest=False, live_version='sha256:' + 'f' * 64)
        self.assertFalse(any(c[0] in ('push', 'tag') for c in self.docker_calls))

    def test_partial_success_retry_reuses_existing_image_without_repushing(self):
        from scripts.image_release import PACKAGE

        def metadata_catches_up(api):
            api.responses[PACKAGE + '/versions?per_page=100&page=1'].append({
                'name': 'sha256:' + 'd' * 64,
                'metadata': {'container': {'tags': ['v1.2.3']}}})

        result, calls = self.publication(existing=False, latest=False,
                                         live_version='sha256:' + 'd' * 64,
                                         after_pull=metadata_catches_up)
        self.assertTrue(result['latest_promoted'])
        self.assertFalse(any(c[0] in ('push', 'tag') for c in calls))

    def test_unknown_or_missing_registry_version_never_causes_blind_push(self):
        for existing, state in ((False, PolicyError('registry denied')),
                                (False, PolicyError('registry timeout')), (True, None)):
            with self.subTest(existing=existing, state=state), self.assertRaises(PolicyError):
                self.publication(existing=existing, live_version=state)
            self.assertFalse(any(c[0] in ('push', 'tag') for c in self.docker_calls))

    def test_metadata_lag_after_partial_success_can_fail_without_repushing(self):
        with self.assertRaises(PolicyError):
            self.publication(existing=False, live_version='sha256:' + 'd' * 64)
        self.assertFalse(any(c[0] in ('push', 'tag') for c in self.docker_calls))

    def test_stale_metadata_cannot_roll_actual_latest_back(self):
        # API still reports v1.2.2; the registry is already on v1.2.4.
        with self.assertRaises(PolicyError):
            self.publication(existing=True, latest=False, live_latest='sha256:' + 'f' * 64)
        self.assertFalse(any(c[:3] == ('buildx', 'imagetools', 'create') for c in self.docker_calls))

    def test_absent_metadata_cannot_hide_an_existing_registry_latest(self):
        with self.assertRaises(PolicyError):
            self.publication(latest=False, metadata_latest=False, live_latest='sha256:' + 'f' * 64)
        self.assertFalse(any(c[:3] == ('buildx', 'imagetools', 'create') for c in self.docker_calls))

    def test_confirmed_first_latest_can_be_created(self):
        result, calls = self.publication(existing=False, latest=False, metadata_latest=False, live_latest=None)
        self.assertTrue(result['latest_promoted'])
        self.assertTrue(any(c[:3] == ('buildx', 'imagetools', 'create') for c in calls))

    def test_metadata_latest_missing_from_registry_is_not_recreated_blindly(self):
        with self.assertRaises(PolicyError):
            self.publication(latest=False, live_latest=None)
        self.assertFalse(any(c[:3] == ('buildx', 'imagetools', 'create') for c in self.docker_calls))

    def test_unknown_registry_state_cannot_promote_latest(self):
        with self.assertRaises(PolicyError):
            self.publication(latest=False, live_latest=PolicyError('registry timeout or denied'))
        self.assertFalse(any(c[:3] == ('buildx', 'imagetools', 'create') for c in self.docker_calls))

    def test_failed_push_does_not_reach_stable_alias(self):
        with self.assertRaises(PolicyError):
            self.publication(existing=False, latest=False, failure='push')


class ImageWorkflowTests(unittest.TestCase):
    def test_workflow_separates_read_only_build_from_trusted_publication(self):
        from pathlib import Path
        import re
        import yaml
        path = Path(__file__).resolve().parents[1] / '.github/workflows/container-image.yml'
        workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
        self.assertEqual(set(workflow['on']), {'pull_request', 'release', 'workflow_dispatch'})
        self.assertEqual(workflow['permissions'], {'contents': 'read'})
        build, publish = workflow['jobs']['build'], workflow['jobs']['publish']
        self.assertNotIn('packages', build['permissions'])
        self.assertEqual(publish['permissions'], {'contents': 'read', 'actions': 'read', 'packages': 'write'})
        self.assertEqual(publish['needs'], 'build')
        self.assertIn("github.repository == 'freesialuo/CalRelay'", publish['if'])
        self.assertIn("github.ref == 'refs/heads/main'", publish['if'])
        self.assertEqual(publish['concurrency']['cancel-in-progress'], 'false')
        for job in (build, publish):
            for step in job['steps']:
                if 'uses' in step:
                    self.assertRegex(step['uses'], r'^[^@]+@[0-9a-f]{40}$')
                self.assertNotIn('secrets', step.get('with', {}))
                self.assertNotIn('build-args', step.get('with', {}))
        self.assertFalse(any('login-action@' in s.get('uses', '') for s in build['steps']))
        docker = next(s['with'] for s in build['steps'] if 'build-push-action@' in s.get('uses', ''))
        self.assertEqual((docker['context'], docker['file'], docker['target']), ('source', 'source/Dockerfile', 'runtime'))
        self.assertEqual((docker['push'], docker['load'], docker['platforms']), ('false', 'true', 'linux/amd64'))
        download = next(s['with'] for s in publish['steps'] if 'download-artifact@' in s.get('uses', ''))
        self.assertEqual(download['artifact-ids'], '${{ needs.build.outputs.artifact }}')
        self.assertNotIn('github-token', download)
        self.assertNotIn('run-id', download)
        self.assertFalse(any('check_docker_image.py' in s.get('run', '') for s in publish['steps']))


class RegistryTagTests(unittest.TestCase):
    def registry(self, final, *, known=None, token_status=200):
        import io
        import json
        from email.message import Message
        import urllib.error
        from scripts.image_release import Registry
        candidate = 'sha256:' + 'd' * 64
        calls = []

        class Response(io.BytesIO):
            def __init__(self, status=200, body=b'', digest=candidate):
                super().__init__(body)
                self.status = status
                self.headers = Message()
                self.headers['Content-Type'] = 'application/vnd.oci.image.manifest.v1+json'
                self.headers['Docker-Content-Digest'] = digest

        class Opener:
            def open(self, request, timeout):
                calls.append((request.full_url, request.get_method()))
                if request.full_url.startswith('https://ghcr.io/token?'):
                    if token_status != 200:
                        raise urllib.error.HTTPError(request.full_url, token_status, 'denied', {}, None)
                    return Response(body=json.dumps({'token': 'synthetic-read-token'}).encode())
                value = final if request.full_url.endswith('/latest') else known
                if isinstance(value, int):
                    raise urllib.error.HTTPError(request.full_url, value, 'registry error', {}, None)
                if isinstance(value, Exception):
                    raise value
                return Response(digest=value if value is not None else candidate)

        registry = Registry('synthetic-workflow-token', 'synthetic-actor')
        registry.opener = Opener()
        return registry, calls, candidate

    def test_head_reads_known_digest_before_actual_latest(self):
        latest = 'sha256:' + 'e' * 64
        registry, calls, candidate = self.registry(latest)
        self.assertEqual(registry.latest_digest(candidate), latest)
        self.assertEqual([method for _, method in calls], ['GET', 'HEAD', 'HEAD'])
        self.assertTrue(calls[1][0].endswith('/' + candidate))
        self.assertTrue(calls[2][0].endswith('/latest'))

    def test_only_authenticated_latest_404_confirms_absence(self):
        registry, calls, candidate = self.registry(404)
        self.assertIsNone(registry.latest_digest(candidate))
        for changes in ({'known': 404}, {'known': 401}, {'token_status': 403}):
            registry, calls, candidate = self.registry(404, **changes)
            with self.subTest(changes=changes), self.assertRaises(PolicyError):
                registry.latest_digest(candidate)
            self.assertFalse(any(url.endswith('/latest') for url, _ in calls))

    def test_version_lookup_reuses_authenticated_head_and_only_404_is_absent(self):
        registry, calls, candidate = self.registry(None)
        self.assertEqual(registry.version_digest('v1.2.3'), candidate)
        self.assertEqual([method for _, method in calls], ['GET', 'HEAD'])
        self.assertTrue(calls[-1][0].endswith('/v1.2.3'))
        registry, _, _ = self.registry(None, known=404)
        self.assertIsNone(registry.version_digest('v1.2.3'))
        for code in (401, 403, 429, 500, 302):
            registry, _, _ = self.registry(None, known=code)
            with self.subTest(code=code), self.assertRaises(PolicyError):
                registry.version_digest('v1.2.3')

    def test_auth_network_redirect_and_malformed_digest_are_unknown(self):
        for result in (401, 403, 429, 500, 302, OSError('timeout'), 'not-a-digest'):
            registry, _, candidate = self.registry(result)
            with self.subTest(result=result), self.assertRaises(PolicyError):
                registry.latest_digest(candidate)


if __name__ == '__main__':
    unittest.main()
