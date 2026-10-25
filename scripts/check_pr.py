"""Validate PR metadata without interpolating untrusted text into shell commands."""

import json
import os
import re
import urllib.request


BRANCH = re.compile(r"^(feat|fix|docs|test|ci|chore|refactor)/([1-9]\d*)-[a-z0-9]+(?:-[a-z0-9]+)*$")
TITLE = re.compile(r"^(feat|fix|docs|test|ci|chore|refactor)(?:\([a-z0-9-]+\))?!?: \S.+$")
CLOSING = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#([1-9]\d*)\b", re.IGNORECASE)


def is_dependabot_pr(pr):
    # These are server-provided fields from the current PR API response, not
    # labels, body text, branch names alone, or the actor rerunning the workflow.
    user = pr.get('user') or {}
    head = pr.get('head') or {}
    head_repo = head.get('repo') or {}
    base_repo = (pr.get('base') or {}).get('repo') or {}
    return (
        user.get('login') == 'dependabot[bot]'
        and user.get('type') == 'Bot'
        and bool(head_repo.get('id'))
        and head_repo['id'] == base_repo.get('id')
        and head.get('ref', '').startswith('dependabot/')
        and len(head['ref']) > len('dependabot/')
    )


def validate_metadata(pr):
    errors = []
    match = BRANCH.fullmatch(pr['head']['ref'])
    if not match and not is_dependabot_pr(pr):
        errors.append('Branch must use <type>/<issue>-<slug>.')
    if not TITLE.fullmatch(pr['title']):
        errors.append('Title must use type(scope): description.')
    if pr['base']['ref'] != 'main':
        errors.append('Base branch must be main.')
    issue = int(match.group(2)) if match else None
    closing = {int(number) for number in CLOSING.findall(pr.get('body') or '')}
    if issue is not None and issue not in closing:
        errors.append('Body must include a closing keyword for the branch Issue.')
    return errors, issue


def github_get(path):
    request = urllib.request.Request(
        'https://api.github.com/' + path,
        headers={
            'Authorization': 'Bearer ' + os.environ['GH_TOKEN'],
            'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2022-11-28',
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def main():
    with open(os.environ['GITHUB_EVENT_PATH'], encoding='utf-8') as stream:
        event = json.load(stream)
    if 'pull_request' not in event:
        print('PR metadata policy does not apply to this event.')
        return
    repo = os.environ['GITHUB_REPOSITORY']
    # Read the current body/title, not a potentially outdated event snapshot.
    pr = github_get(f"repos/{repo}/pulls/{event['number']}")
    errors, issue = validate_metadata(pr)
    if issue is not None:
        record = github_get(f'repos/{repo}/issues/{issue}')
        if 'pull_request' in record:
            errors.append('The linked number must identify an Issue, not a PR.')
    if errors:
        raise SystemExit('\n'.join(errors))
    if issue is None:
        print('Dependabot PR metadata validated (no manual Issue required).')
    else:
        print(f'PR metadata validated against Issue #{issue}.')


if __name__ == '__main__':
    main()
