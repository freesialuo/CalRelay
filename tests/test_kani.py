"""The verification gate must distinguish counterexamples from broken tooling."""
import unittest

from scripts.check_kani import verify_output


class KaniGateTests(unittest.TestCase):
    def test_positive_run_requires_all_harnesses_and_successful_exit(self):
        summary = 'Complete - 3 successfully verified harnesses, 0 failures, 3 total.'
        verify_output(summary, 0)
        for output, code in [(summary, 1), ('Complete - 0 successfully verified harnesses', 0)]:
            with self.subTest(output=output, code=code), self.assertRaises(RuntimeError):
                verify_output(output, code)

    def test_mutant_requires_the_named_failed_assertion(self):
        output = self.counterexample('expected assertion')
        verify_output(output, 1, ('expected assertion',))
        for changed, code in [
            (output, 0),
            ('error: compilation failed', 1),
            (output.replace('expected assertion', 'unwinding assertion loop 0'), 1),
            (output.replace('Status: FAILURE', 'Status: SUCCESS'), 1),
            (output.replace('Description:', 'Missing:'), 1),
            (output.replace('1 failures, 1 total.', '2 failures, 2 total.'), 1),
            (output + self.counterexample('unexpected assertion'), 1),
        ]:
            with self.subTest(output=changed, code=code), self.assertRaises(RuntimeError):
                verify_output(changed, code, ('expected assertion',))

    @staticmethod
    def counterexample(message):
        return (f'Check 1: proof.assertion.1\n - Status: FAILURE\n'
                f' - Description: "{message}"\n\nSUMMARY:\n'
                'VERIFICATION:- FAILED\n'
                'Complete - 0 successfully verified harnesses, 1 failures, 1 total.\n')


if __name__ == '__main__':
    unittest.main()
