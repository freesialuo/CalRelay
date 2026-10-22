"""Prove recurrence helpers, then require real assertion counterexamples in source mutants."""

from pathlib import Path
import os
import re
import signal
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = Path('crates/calendar-domain/src/recurrence.rs')


def verify_output(output, returncode, expected_failures=None):
    if expected_failures is None:
        if returncode or 'Complete - 3 successfully verified harnesses, 0 failures, 3 total.' not in output:
            raise RuntimeError('Expected all three proof harnesses to pass')
        return
    checks = re.findall(r'Check \d+:.*?(?=\nCheck |\n\nSUMMARY:)', output, re.S)
    failures = [check for check in checks if 'Status: FAILURE' in check]
    descriptions = set()
    for check in failures:
        description = re.search(r'- Description: "(.*)"', check)
        descriptions.add(description.group(1).strip('"') if description else None)
    if (returncode == 0 or 'VERIFICATION:- FAILED' not in output
            or not failures or '1 failures, 1 total.' not in output
            or descriptions != set(expected_failures)):
        raise RuntimeError('Expected named assertion counterexamples, not a build/tool/unwind failure')


def run(command, cwd, *, expected_failures=None):
    with subprocess.Popen(command, cwd=cwd, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, start_new_session=True) as process:
        try:
            output, _ = process.communicate(timeout=300)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            output, _ = process.communicate()
            print(output, flush=True)
            raise RuntimeError('Kani timed out; verification did not pass') from None
        print(output, flush=True)
        verify_output(output, process.returncode, expected_failures)


def main():
    version = (ROOT / 'scripts/kani-version').read_text().strip()
    installed = subprocess.check_output(['cargo', 'kani', '--version'], text=True, timeout=30).strip()
    if not re.search(rf'^Kani Rust Verifier {re.escape(version)}(?: |$)', installed):
        raise RuntimeError(f'Expected Kani {version}, got {installed}')
    command = ['cargo', 'kani', '-p', 'calendar-domain', '--lib']
    # All harnesses must pass before any mutant can count as a detected defect.
    run(command, ROOT)
    mutants = [
        ('budget', '.checked_sub(1)', '.checked_sub(0)', 'budget_step',
         ('budget must decrease by one', 'exhausted budget must reject')),
        ('window', 'start_utc < window_end && end_utc > window_start',
         'start_utc <= window_end && end_utc > window_start', 'window_membership',
         ('half-open window membership must match specification',)),
    ]
    temporary = Path(tempfile.mkdtemp(prefix='calrelay-kani-'))
    try:
        # A separate workspace keeps the user's sources untouched even on timeout/failure.
        for name in ['Cargo.toml', 'Cargo.lock']:
            shutil.copy2(ROOT / name, temporary / name)
        shutil.copytree(ROOT / 'crates', temporary / 'crates')
        original = (temporary / SOURCE).read_text()
        for name, before, after, harness, assertion in mutants:
            if original.count(before) != 1:
                raise RuntimeError(f'Mutation anchor changed: {name}')
            (temporary / SOURCE).write_text(original.replace(before, after))
            print(f'Checking {name} mutation', flush=True)
            run(command + ['--harness', harness], temporary, expected_failures=assertion)
            (temporary / SOURCE).write_text(original)
    finally:
        # Repository convention: deletions use find, including temporary workspace cleanup.
        subprocess.run(['find', str(temporary), '-depth', '-delete'], check=True)
    print('All proofs passed; both source mutants produced assertion counterexamples.')


if __name__ == '__main__':
    main()
