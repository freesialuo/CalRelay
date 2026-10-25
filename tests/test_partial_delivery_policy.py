"""PR gate regressions for explicit partial delivery, using current API records."""
from copy import deepcopy
import json
import os
import tempfile
import unittest
from unittest.mock import call, patch
from urllib.error import HTTPError

from scripts.check_pr import main, validate_metadata


class PartialDeliveryPolicyTests(unittest.TestCase):
    def setUp(self):
        self.pr = {
            'head': {'ref': 'feat/36-core-supervision'},
            'base': {'ref': 'main'},
            'title': 'feat(ops): expose core health foundations',
            'body': ('Delivery: partial\nRefs #36\nCloses #86\n'
                     'Remaining AC: #36 AC1, AC2, AC3 — production runtime integration\n'),
        }
        self.parent = {'number': 36, 'state': 'open',
                       'body': '- [ ] AC1: workers\n- [ ] AC2: health\n- [ ] AC3: CLI'}
        self.leaf = {'number': 86, 'state': 'open',
                     'sub_issues_summary': {'total': 0}}

    def run_main(self, pr=None, parent=None, leaf=None, pr_number=71):
        records = [self.pr if pr is None else pr,
                   self.parent if parent is None else parent,
                   self.leaf if leaf is None else leaf]
        with tempfile.NamedTemporaryFile(mode='w+', encoding='utf-8') as event:
            # The event body is intentionally obsolete; only current API metadata counts.
            json.dump({'number': pr_number, 'pull_request': {'body': 'Closes #36'}}, event)
            event.flush()
            with patch.dict(os.environ, GITHUB_EVENT_PATH=event.name,
                            GITHUB_REPOSITORY='owner/repo'):
                with patch('scripts.check_pr.github_get', side_effect=records) as get:
                    with patch('builtins.print'):
                        main()
                    return get.call_args_list

    def test_explicit_partial_delivery_keeps_parent_open(self):
        self.assertEqual(validate_metadata(self.pr), ([], 36))
        self.assertEqual(self.run_main(), [
            call('repos/owner/repo/pulls/71'),
            call('repos/owner/repo/issues/36'),
            call('repos/owner/repo/issues/86'),
        ])

    def test_different_pr_branch_and_leaf_ids_are_not_hard_coded(self):
        self.pr['head']['ref'] = 'feat/75-due-delivery-foundation'
        self.pr['body'] = self.pr['body'].replace('#36', '#75').replace('#86', '#90')
        self.parent['number'] = 75
        self.leaf['number'] = 90
        self.assertEqual(validate_metadata(self.pr), ([], 75))
        self.assertEqual(self.run_main(pr_number=88), [
            call('repos/owner/repo/pulls/88'),
            call('repos/owner/repo/issues/75'),
            call('repos/owner/repo/issues/90'),
        ])

    def test_keyword_colon_and_qualified_closing_cannot_close_parent(self):
        self.pr['body'] = self.pr['body'].replace('Closes #86', 'Closes: #86')
        self.assertEqual(validate_metadata(self.pr), ([], 36))
        for extra in ('Closes: #36', 'Fixes owner/repo#36',
                      'Resolves https://github.com/owner/repo/issues/36',
                      'Closes other/repo#90'):
            with self.subTest(extra=extra):
                pr = deepcopy(self.pr)
                pr['body'] += '\n' + extra
                self.assertTrue(validate_metadata(pr)[0])

    def test_malformed_or_duplicate_mode_fields_cannot_fall_back_to_full(self):
        for mode in ('partial extra', '', 'full extra'):
            with self.subTest(mode=mode):
                pr = deepcopy(self.pr)
                pr['body'] = 'Delivery: ' + mode + '\nCloses #36'
                self.assertTrue(validate_metadata(pr)[0])
        self.pr['body'] += '\nRemaining AC: malformed'
        self.assertTrue(validate_metadata(self.pr)[0])

    def test_partial_cannot_close_parent_even_with_a_leaf(self):
        self.pr['body'] += '\nCloses #36'
        self.assertTrue(validate_metadata(self.pr)[0])

    def test_missing_ambiguous_or_untraceable_partial_fields_are_rejected(self):
        changes = [
            ('Delivery: partial\n', ''),
            ('Refs #36', 'Refs #37'),
            ('Closes #86', ''),
            ('Closes #86', 'Closes #86\nCloses #87'),
            ('Remaining AC: #36 AC1, AC2, AC3 — production runtime integration', ''),
            ('Remaining AC: #36', 'Remaining AC: #37'),
            ('AC1, AC2, AC3 — production runtime integration', 'AC1'),
            ('Delivery: partial', 'Delivery: unknown'),
            ('Delivery: partial', 'Delivery: partial\nDelivery: full'),
            ('Refs #36', 'Refs owner/another-repo#36'),
        ]
        for old, new in changes:
            with self.subTest(old=old, new=new):
                pr = deepcopy(self.pr)
                pr['body'] = pr['body'].replace(old, new)
                self.assertTrue(validate_metadata(pr)[0])

    def test_hidden_partial_fields_or_closings_do_not_satisfy_the_gate(self):
        for field in ('Delivery: partial', 'Refs #36', 'Closes #86',
                      'Remaining AC: #36 AC1, AC2, AC3 — production runtime integration'):
            for wrap in ('<!--\n{}\n-->', '```text\n{}\n```', '~~~\n{}\n~~~',
                         '> {}', '    {}', '`{}`'):
                with self.subTest(field=field, wrap=wrap):
                    pr = deepcopy(self.pr)
                    pr['body'] = pr['body'].replace(field, wrap.format(field))
                    self.assertTrue(validate_metadata(pr)[0])
        self.pr['body'] += '\n<!-- Closes #36 -->'
        self.assertTrue(validate_metadata(self.pr)[0])

    def test_full_delivery_still_requires_visible_branch_closing(self):
        for body in ('Refs #36\nCloses #86', '<!-- Closes #36 -->',
                     '```\nCloses #36\n```', '> Closes #36'):
            with self.subTest(body=body):
                pr = deepcopy(self.pr)
                pr['body'] = body
                self.assertTrue(validate_metadata(pr)[0])
        self.pr['body'] = 'Delivery: full\nCloses #36'
        self.assertEqual(validate_metadata(self.pr), ([], 36))

    def test_partial_keeps_title_branch_and_base_checks(self):
        for change in ({'title': 'unstructured'}, {'base': {'ref': 'release'}},
                       {'head': {'ref': 'feature/untracked'}}):
            with self.subTest(change=change):
                pr = deepcopy(self.pr)
                pr.update(change)
                self.assertTrue(validate_metadata(pr)[0])

    def test_real_native_parent_is_checked_when_available(self):
        self.leaf['parent_issue_url'] = 'https://api.github.com/repos/owner/repo/issues/36'
        self.run_main()
        self.leaf['parent_issue_url'] = 'https://api.github.com/repos/owner/repo/issues/7'
        with self.assertRaisesRegex(SystemExit, 'native parent'):
            self.run_main()

    def test_leaf_must_be_an_open_real_issue_without_children(self):
        for change in ({'pull_request': {}}, {'state': 'closed'},
                       {'number': 99}, {'sub_issues_summary': {'total': 1}},
                       {'sub_issues_summary': None}, {'sub_issues_summary': {}},
                       {'sub_issues_summary': {'total': False}}):
            with self.subTest(change=change):
                leaf = deepcopy(self.leaf)
                leaf.update(change)
                with self.assertRaises(SystemExit):
                    self.run_main(leaf=leaf)

    def test_remaining_ac_must_exist_on_open_parent_issue(self):
        for parent in ({**self.parent, 'state': 'closed'},
                       {**self.parent, 'body': '- [ ] AC1: workers'},
                       {**self.parent, 'pull_request': {}}):
            with self.subTest(parent=parent):
                with self.assertRaises(SystemExit):
                    self.run_main(parent=parent)

    def test_missing_leaf_api_record_fails_closed(self):
        with self.assertRaises(HTTPError):
            self.run_main(leaf=HTTPError('https://api.github.com/example', 404,
                                        'Not Found', None, None))


if __name__ == '__main__':
    unittest.main()
