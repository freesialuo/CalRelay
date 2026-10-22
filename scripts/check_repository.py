"""Check documentation links and workflow invariants for the design-stage repo."""

from pathlib import Path
import json
import re

import yaml


ROOT = Path(__file__).resolve().parents[1]


def main():
    for document in [ROOT / 'README.md', ROOT / 'CONTRIBUTING.md', *(ROOT / 'docs').rglob('*.md')]:
        for target in re.findall(r'\]\(([^)]+)\)', document.read_text()):
            if '://' in target or target.startswith('#'):
                continue
            path = target.split('#')[0]
            assert (document.parent / path).exists(), f'{document}: missing {target}'
    for document in (ROOT / '.github').rglob('*.yml'):
        yaml.safe_load(document.read_text())
    for document in [*(ROOT / 'schemas').glob('*.json'), *(ROOT / 'examples').glob('*.json')]:
        json.loads(document.read_text())
    # BaseLoader retains YAML 1.2's `on` spelling without YAML 1.1 boolean conversion.
    workflow = yaml.load((ROOT / '.github/workflows/ci.yml').read_text(), Loader=yaml.BaseLoader)
    assert 'pull_request' in workflow['on'] and 'push' in workflow['on']
    assert 'pull_request_target' not in workflow['on']
    assert workflow['permissions'] == {'contents': 'read'}
    assert set(workflow['jobs']['quality-gate']['needs']) == {'contracts', 'pr-policy', 'rust', 'kani'}
    for job in workflow['jobs'].values():
        for step in job.get('steps', []):
            if 'uses' in step:
                assert re.fullmatch(r'[^@]+@[0-9a-f]{40}', step['uses']), step['uses']
    rules = json.loads((ROOT / '.github/main-ruleset.json').read_text())
    assert rules['enforcement'] == 'active' and rules['bypass_actors'] == []
    assert rules['conditions']['ref_name']['include'] == ['refs/heads/main']
    pull_request = next(rule['parameters'] for rule in rules['rules'] if rule['type'] == 'pull_request')
    assert pull_request['required_approving_review_count'] == 0
    assert pull_request['require_code_owner_review'] is False
    assert pull_request['require_last_push_approval'] is False
    print('Repository links, YAML, JSON, Action pins and main policy checked.')


if __name__ == '__main__':
    main()
