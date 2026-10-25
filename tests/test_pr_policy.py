from copy import deepcopy
import json
import os
import tempfile
import unittest
from unittest.mock import patch

from scripts.check_pr import main, validate_metadata


class PullRequestPolicyTests(unittest.TestCase):
    def setUp(self):
        self.pr = {
            'head': {'ref': 'feat/12-ics-ingestion'},
            'base': {'ref': 'main'},
            'title': 'feat(calendar): ingest authenticated ICS',
            'body': 'Adds source ingestion.\n\nCloses #12',
        }

    def test_valid_issue_branch_and_closing_reference(self):
        self.assertEqual(validate_metadata(self.pr), ([], 12))

    def test_wrong_issue_invalid_branch_base_and_title_are_rejected(self):
        changes = [
            {'body': 'Closes #13'},
            {'body': None},
            {'body': 'Closes owner/another-repo#12'},
            {'head': {'ref': 'feature/untracked-change'}},
            {'base': {'ref': 'release'}},
            {'title': 'unstructured title'},
        ]
        for change in changes:
            with self.subTest(change=change):
                pr = deepcopy(self.pr)
                pr.update(change)
                self.assertTrue(validate_metadata(pr)[0])

    def test_shell_metacharacters_are_only_metadata(self):
        self.pr['title'] = 'feat: handle $(untrusted) input safely'
        self.assertEqual(validate_metadata(self.pr), ([], 12))


class DependabotPolicyTests(unittest.TestCase):
    def setUp(self):
        self.pr = {
            'user': {'login': 'dependabot[bot]', 'type': 'Bot'},
            'head': {'ref': 'dependabot/cargo/compatible-dependencies-123',
                     'repo': {'id': 42}},
            'base': {'ref': 'main', 'repo': {'id': 42}},
            'title': 'chore(deps): bump compatible dependencies',
            'body': 'Automated dependency update.',
        }

    def test_genuine_dependabot_needs_no_issue(self):
        for ecosystem in ('cargo', 'pip', 'github_actions'):
            with self.subTest(ecosystem=ecosystem):
                self.pr['head']['ref'] = f'dependabot/{ecosystem}/some-update'
                self.assertEqual(validate_metadata(self.pr), ([], None))

    def test_untrusted_identity_or_branch_has_no_exception(self):
        changes = [
            ('user', None),
            ('user', {}),
            ('user', {'login': 'dependabot[bot]', 'type': 'User'}),
            ('user', {'login': 'someone', 'type': 'Bot'}),
            ('head', {'ref': 'dependabot/cargo/update', 'repo': {'id': 43}}),
            ('head', {'ref': 'dependabot/cargo/update', 'repo': None}),
            ('head', {'ref': 'dependabot/cargo/update'}),
            ('head', {'ref': 'dependabot-imposter/update', 'repo': {'id': 42}}),
            ('head', {'ref': 'dependabot/', 'repo': {'id': 42}}),
            ('base', {'ref': 'main', 'repo': None}),
        ]
        for key, value in changes:
            with self.subTest(key=key, value=value):
                pr = deepcopy(self.pr)
                pr[key] = value
                self.assertIn('Branch must use <type>/<issue>-<slug>.',
                              validate_metadata(pr)[0])

    def test_dependabot_still_checks_title_and_base(self):
        self.pr['title'] = 'Bump a dependency'
        self.pr['base']['ref'] = 'release'
        self.assertEqual(validate_metadata(self.pr), ([
            'Title must use type(scope): description.',
            'Base branch must be main.',
        ], None))

    def test_bot_on_normal_issue_branch_still_needs_closing_reference(self):
        self.pr['head']['ref'] = 'chore/12-dependency-update'
        self.assertEqual(validate_metadata(self.pr), ([
            'Body must include a closing keyword for the branch Issue.',
        ], 12))

    def test_main_uses_current_api_author_not_event_sender(self):
        with tempfile.NamedTemporaryFile(mode='w+', encoding='utf-8') as event:
            json.dump({'number': 7, 'pull_request': {},
                       'sender': {'login': 'human-rerunning-ci'}}, event)
            event.flush()
            with patch.dict(os.environ, GITHUB_EVENT_PATH=event.name,
                            GITHUB_REPOSITORY='owner/repo'):
                with patch('scripts.check_pr.github_get', return_value=self.pr) as get:
                    with patch('builtins.print'):
                        main()
                    get.assert_called_once_with('repos/owner/repo/pulls/7')

    def test_main_keeps_linked_issue_type_check(self):
        self.pr['head']['ref'] = 'chore/12-dependency-update'
        self.pr['body'] = 'Closes #12'
        with tempfile.NamedTemporaryFile(mode='w+', encoding='utf-8') as event:
            json.dump({'number': 7, 'pull_request': {}}, event)
            event.flush()
            with patch.dict(os.environ, GITHUB_EVENT_PATH=event.name,
                            GITHUB_REPOSITORY='owner/repo'):
                with patch('scripts.check_pr.github_get', side_effect=[
                    self.pr, {'pull_request': {}},
                ]):
                    with self.assertRaisesRegex(SystemExit, 'Issue, not a PR'):
                        main()


if __name__ == '__main__':
    unittest.main()
