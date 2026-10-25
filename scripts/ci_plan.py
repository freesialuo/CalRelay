"""Conservative PR routing; main/manual stay full and metadata reuses exact-head evidence."""
import json
import os
from pathlib import Path
import subprocess

from scripts.image_release import GitHub, PolicyError, PREFIX, REPOSITORY, SHA, outputs, require


def light(path):
    return (path in ('README.md', 'CONTRIBUTING.md', 'AGENTS.md', '.github/pull_request_template.md')
            or path.startswith('docs/') and path.endswith('.md') or path.startswith('.github/ISSUE_TEMPLATE/')
            or path.startswith(('scripts/', 'tests/')) and path.endswith('.py')
            or path == 'requirements/ci.txt')


def relevant(paths):
    if paths is None:
        return {'rust': True, 'frontend': True}
    result = {'rust': False, 'frontend': False}
    for path in paths:
        if path in ('.github/workflows/ci.yml', 'scripts/ci_plan.py', 'tests/test_ci_plan.py'):
            return {'rust': True, 'frontend': True}
        if path.startswith('web/tests/') or path.startswith('web/playwright'):
            result['frontend'] = True
        elif not light(path) and path != '.github/workflows/container-image.yml':
            return {'rust': True, 'frontend': True}  # Includes unknown inputs and shared Rust/web contracts.
    return result


def image_relevant(paths):
    if paths is None:
        return True
    for path in paths:
        if path in ('Dockerfile', '.dockerignore', 'compose.yaml', 'scripts/check_docker_image.py',
                    'examples/docker-deployment.json', '.github/workflows/container-image.yml'):
            return True
        if light(path) or path.startswith(('.github/', 'web/tests/')) or path.startswith('web/playwright'):
            continue
        if path.startswith('crates/') and '/tests/' in path:
            continue  # Explicitly excluded by the production build context.
        return True
    return False


def changed_paths(event):
    pr = event.get('pull_request') or {}
    base, head = (pr.get('base') or {}).get('sha'), (pr.get('head') or {}).get('sha')
    if not all(isinstance(x, str) and SHA.fullmatch(x) for x in (base, head)):
        return None
    try:
        result = subprocess.run(['git', 'diff', '--name-only', '--no-renames', '-z', base, head],
                                check=True, capture_output=True, timeout=20)
        return [p for p in result.stdout.decode('utf-8').split('\0') if p]
    except (OSError, subprocess.SubprocessError, UnicodeError):
        return None  # Missing shallow objects never mean an empty diff.


def receipt(api, event, run_id, needed, merge_sha):
    """Only the newest prior run of this workflow/head/repository can supply code evidence."""
    require(isinstance(merge_sha, str) and SHA.fullmatch(merge_sha), 'Unknown tested merge revision.')
    pr = event['pull_request']
    head = pr['head']
    current = api.get(PREFIX + f'/actions/runs/{run_id}')
    workflow = current.get('workflow_id')
    require(type(workflow) is int and current.get('path') == '.github/workflows/ci.yml', 'Unknown CI workflow.')
    response = api.get(PREFIX + f'/actions/runs?event=pull_request&head_sha={head["sha"]}&per_page=100')
    require(response.get('total_count', 0) <= 100, 'Excessive CI history; run full checks.')
    runs = [r for r in response.get('workflow_runs', []) if r.get('id') != run_id and r.get('workflow_id') == workflow]
    require(bool(runs), 'No previous CI evidence.')
    run = max(runs, key=lambda r: (r.get('run_number', 0), r.get('run_attempt', 0)))
    require(run.get('head_sha') == head['sha'] and run.get('head_branch') == head['ref']
            and run.get('event') == 'pull_request'
            and (run.get('repository') or {}).get('full_name') == REPOSITORY
            and (run.get('head_repository') or {}).get('full_name') == (head.get('repo') or {}).get('full_name')
            and (head.get('repo') or {}).get('full_name')
            and run.get('status') == 'completed' and run.get('conclusion') == 'success', 'Latest code evidence is not successful.')
    response = api.get(PREFIX + f'/actions/runs/{run["id"]}/jobs?filter=latest&per_page=100')
    require(response.get('total_count', 0) <= 100, 'Excessive CI jobs.')
    for name in [f'checks ({merge_sha})', 'quality-gate', *[key for key, value in needed.items() if value]]:
        jobs = [j for j in response.get('jobs', []) if j.get('name') == name]
        require(len(jobs) == 1 and jobs[0].get('status') == 'completed'
                and jobs[0].get('conclusion') == 'success', 'Missing successful relevant check.')
    fresh = api.get(PREFIX + f'/actions/runs/{run["id"]}')
    require(fresh.get('run_attempt') == run.get('run_attempt') and fresh.get('status') == 'completed'
            and fresh.get('conclusion') == 'success', 'Previous CI changed during validation.')
    return {'id': run['id'], 'attempt': run['run_attempt']}


def plan(event_name, event, paths, api=None, run_id=0, merge_sha=None):
    needed = relevant(paths) if event_name == 'pull_request' else {'rust': True, 'frontend': True}
    result = {**needed, 'proof': 'paths' if event_name == 'pull_request' else 'full', 'receipt_id': '', 'receipt_attempt': ''}
    if event_name == 'pull_request' and event.get('action') == 'edited' and any(needed.values()) and api:
        try:
            previous = receipt(api, event, run_id, needed, merge_sha)
            result.update(rust=False, frontend=False, proof='receipt', receipt_id=previous['id'], receipt_attempt=previous['attempt'])
        except (PolicyError, KeyError, TypeError, ValueError):
            result.update(rust=True, frontend=True, proof='full')  # Unknown/stale evidence cannot skip code.
    return result


def gate(route, checks, results, needed, previous=None):
    require(checks == 'success', 'Fast checks/routing failed or were cancelled.')
    require(route.get('proof') in ('paths', 'full', 'receipt'), 'Unknown routing proof.')
    for name in ('rust', 'frontend'):
        require(type(route[name]) is bool, 'Invalid route flag.')
        allowed = ('success',) if route[name] else ('success', 'skipped')
        require(results.get(name) in allowed, 'Relevant check failed, cancelled or was unsafely skipped.')
    if route['proof'] == 'receipt':
        require(previous == {'id': int(route['receipt_id']), 'attempt': int(route['receipt_attempt'])}, 'Code evidence changed.')
    else:
        require(all(route[name] == needed[name] for name in needed), 'Skip is not justified by the change set.')


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('plan', 'gate', 'image'))
    command = parser.parse_args().command
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    event_name = os.environ['GITHUB_EVENT_NAME']
    paths = changed_paths(event) if event_name == 'pull_request' else None
    if command == 'image':
        outputs({'run_image': event_name != 'pull_request' or image_relevant(paths)})
        return
    run_id = int(os.environ['GITHUB_RUN_ID'])
    api = GitHub(os.environ.get('GH_TOKEN')) if os.environ.get('GITHUB_REPOSITORY') == REPOSITORY else None
    if command == 'plan':
        result = plan(event_name, event, paths, api, run_id, os.environ.get('GITHUB_SHA'))
        outputs({**result, 'route': json.dumps(result, separators=(',', ':'))})
        print(json.dumps(result, sort_keys=True))
        return
    route = json.loads(os.environ['CI_ROUTE'])
    needed = relevant(paths) if event_name == 'pull_request' else {'rust': True, 'frontend': True}
    previous = receipt(api, event, run_id, needed, os.environ.get('GITHUB_SHA')) if route.get('proof') == 'receipt' else None
    gate(route, os.environ['CHECKS_RESULT'], {key: os.environ[key.upper() + '_RESULT'] for key in needed}, needed, previous)
    print('All relevant checks passed; skips have explicit path or exact-head CI evidence.')


if __name__ == '__main__':
    try:
        main()
    except (PolicyError, KeyError, TypeError, ValueError) as error:
        raise SystemExit(str(error) if isinstance(error, PolicyError) else 'Invalid CI route; refusing to pass.') from None
