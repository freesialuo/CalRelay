"""Validate PR metadata without interpolating untrusted text into shell commands."""

import json
import os
import re
import urllib.request


BRANCH = re.compile(r"^(feat|fix|docs|test|ci|chore|refactor)/([1-9]\d*)-[a-z0-9]+(?:-[a-z0-9]+)*$")
TITLE = re.compile(r"^(feat|fix|docs|test|ci|chore|refactor)(?:\([a-z0-9-]+\))?!?: \S.+$")
CLOSING_WORD = r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)"
CLOSING = re.compile(CLOSING_WORD + r":?\s+#([1-9]\d*)\b", re.IGNORECASE)
NONLOCAL_CLOSING = re.compile(
    CLOSING_WORD + r":?\s+(?:[\w.-]+/[\w.-]+#[1-9]\d*|"
    r"https?://github\.com/[^/\s]+/[^/\s]+/(?:issues|pull)/[1-9]\d*)",
    re.IGNORECASE,
)
MODE = re.compile(r"^Delivery:[ \t]*([^\n]*)$", re.IGNORECASE | re.MULTILINE)
REFS = re.compile(r"^Refs[ \t]+#([1-9]\d*)[ \t]*$", re.IGNORECASE | re.MULTILINE)
REMAINING = re.compile(
    r"^Remaining AC:[ \t]*#([1-9]\d*)[ \t]+"
    r"(AC[1-9]\d*(?:[ \t]*,[ \t]*AC[1-9]\d*)*)"
    r"[ \t]+(?:—|-|:)[ \t]+(\S[^\n]*)$", re.IGNORECASE | re.MULTILINE,
)


def visible_body(body):
    """Policy fields must be readable prose, not comments, code, or quotations."""
    body = re.sub(r'<!--.*?(?:-->|\Z)', '', body, flags=re.DOTALL)
    lines, fence = [], None
    for line in body.splitlines():
        marker = re.match(r'^ {0,3}(`{3,}|~{3,})', line)
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence):
                fence = None
            continue
        if marker:
            fence = marker[1]
            continue
        if line.startswith(('    ', '\t')) or line.lstrip().startswith('>'):
            continue
        lines.append(re.sub(r'`+[^`]*`+', '', line).strip())
    return '\n'.join(lines)


def delivery_fields(pr):
    body = visible_body(pr.get('body') or '')
    modes = [mode.strip().lower() for mode in MODE.findall(body)]
    closing = {int(number) for number in CLOSING.findall(body)}
    return body, modes, closing, REMAINING.findall(body)


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
    body, modes, closing, remaining = delivery_fields(pr)
    if modes and modes not in (['full'], ['partial']):
        errors.append('Delivery must declare exactly one full or partial mode.')
    if modes == ['partial']:
        if issue is None:
            errors.append('Partial delivery requires a tracked Issue branch.')
        if [int(n) for n in REFS.findall(body)].count(issue) != 1:
            errors.append('Partial delivery must explicitly Refs the branch Issue once.')
        # Even a hidden closing reference must not accidentally close the parent.
        raw_closing = {int(n) for n in CLOSING.findall(pr.get('body') or '')}
        if issue in raw_closing:
            errors.append('Partial delivery must not close the branch Issue.')
        if NONLOCAL_CLOSING.search(pr.get('body') or ''):
            errors.append('Partial closing references must use local #Issue syntax.')
        if len(closing) != 1 or issue in closing:
            errors.append('Partial delivery must close exactly one different leaf Issue.')
        remaining_lines = re.findall(r'^Remaining AC:[^\n]*$', body, re.IGNORECASE | re.MULTILINE)
        if len(remaining_lines) != 1 or len(remaining) != 1 or int(remaining[0][0]) != issue:
            errors.append('Partial delivery must list Remaining AC for the branch Issue with a reason.')
    elif issue is not None and issue not in closing:
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


def validate_partial_records(repo, issue, pr, parent):
    """Validate actual current records; a logical reference does not create native links."""
    _, _, closing, remaining = delivery_fields(pr)
    errors = []
    leaf_number = next(iter(closing))  # Called only after metadata validation.
    leaf = github_get(f'repos/{repo}/issues/{leaf_number}')
    if 'pull_request' in leaf:
        errors.append('The closing target must identify an Issue, not a PR.')
    if leaf.get('number') != leaf_number or leaf.get('state') != 'open':
        errors.append('Partial delivery must close the identified open leaf Issue.')
    children = (leaf.get('sub_issues_summary') or {}).get('total')
    if type(children) is not int or children != 0:
        errors.append('Partial delivery requires a verified leaf Issue without sub-issues.')
    native_parent = leaf.get('parent_issue_url')
    if native_parent and native_parent != f'https://api.github.com/repos/{repo}/issues/{issue}':
        errors.append('The leaf native parent conflicts with the branch Issue.')
    known_ac = set(re.findall(r'\bAC([1-9]\d*)\b', parent.get('body') or '', re.IGNORECASE))
    remaining_ac = set(re.findall(r'AC([1-9]\d*)', remaining[0][1], re.IGNORECASE))
    if parent.get('state') != 'open' or not remaining_ac.issubset(known_ac):
        errors.append('Remaining AC must exist on the open branch Issue.')
    return errors


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
        if not errors and delivery_fields(pr)[1] == ['partial']:
            errors.extend(validate_partial_records(repo, issue, pr, record))
    if errors:
        raise SystemExit('\n'.join(errors))
    if issue is None:
        print('Dependabot PR metadata validated (no manual Issue required).')
    elif delivery_fields(pr)[1] == ['partial']:
        leaf = next(iter(delivery_fields(pr)[2]))
        print(f'Partial PR metadata validated: Refs #{issue}, closes leaf #{leaf}.')
    else:
        print(f'PR metadata validated against Issue #{issue}.')


if __name__ == '__main__':
    main()
