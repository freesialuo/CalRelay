from copy import deepcopy
import unittest

from scripts.check_pr import validate_metadata


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


if __name__ == '__main__':
    unittest.main()
