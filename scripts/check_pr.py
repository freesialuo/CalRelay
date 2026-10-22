"""Validate PR metadata without interpolating untrusted text into shell commands."""

import json
import os
import re
import urllib.request


BRANCH = re.compile(r"^(feat|fix|docs|test|ci|chore|refactor)/([1-9]\d*)-[a-z0-9]+(?:-[a-z0-9]+)*$")
TITLE = re.compile(r"^(feat|fix|docs|test|ci|chore|refactor)(?:\([a-z0-9-]+\))?!?: \S.+$")
CLOSING = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#([1-9]\d*)\b", re.IGNORECASE)


def validate_metadata(pr):
    errors = []
    match = BRANCH.fullmatch(pr['head']['ref'])
    if not match:
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
    print(f'PR metadata validated against Issue #{issue}.')


if __name__ == '__main__':
    main()
