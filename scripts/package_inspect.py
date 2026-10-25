"""Read-only diagnosis of the first alpha publication; never publish or repair settings."""
import json
import os
from pathlib import Path
import subprocess

from scripts.image_release import (DIGEST, IMAGE, PACKAGE, PREFIX, REPOSITORY, SHA,
                                  GitHub, PolicyError, Registry, event_context,
                                  quality_gate, qualify_source, require, require_main_ancestor)

VERSION = 'v0.1.0-alpha.1'
SOURCE = 'acdc38e286eb141eab017174840c7c993899cd56'
EXPECTED_IMAGE = 'sha256:5e733399bf33e9deee716c96bb605eb741f952c477553e6a45aeb4085e22b0c2'
REF = 'refs/heads/release-inspect/' + VERSION


def qualify_inspection(api, context):
    event, sha = context['event'], context['sha']
    require(context['event_name'] == 'push' and context['ref'] == REF
            and (event.get('repository') or {}).get('full_name') == REPOSITORY
            and SHA.fullmatch(sha) and context['workflow_sha'] == sha
            and event.get('after') == sha and event.get('deleted') is False
            and event.get('forced') is False, 'Invalid inspection context.')
    ref = api.get(PREFIX + '/git/ref/' + REF[len('refs/'):])
    require(ref.get('ref') == REF and (ref.get('object') or {}).get('type') == 'commit'
            and (ref.get('object') or {}).get('sha') == sha, 'Inspection request changed.')
    require_main_ancestor(api, sha)
    quality_gate(api, sha)
    source, _ = qualify_source(api, VERSION)
    require(source == SOURCE, 'Approved alpha tag moved; inspection stopped.')


def metadata_facts(api):
    package = api.get(PACKAGE, missing=True)
    facts = {'version': VERSION, 'source': SOURCE,
             'package_lookup': 'found' if package is not None else '404_unknown_or_absent'}
    if package is None:
        return facts
    repository = package.get('repository')
    facts.update(
        package_fields=[key for key in ('name', 'visibility', 'repository', 'created_at', 'updated_at') if key in package],
        visibility=package.get('visibility') if package.get('visibility') in ('public', 'private', 'internal') else 'unknown',
        repository_type=type(repository).__name__,
        repository_fields=[key for key in ('full_name', 'id', 'html_url', 'private') if isinstance(repository, dict) and key in repository],
        repository_matches_expected=isinstance(repository, dict) and repository.get('full_name') == REPOSITORY,
        expected_repository=REPOSITORY,
        package_created_at=package.get('created_at'), package_updated_at=package.get('updated_at'),
    )
    versions = api.get(PACKAGE + '/versions?per_page=100&page=1')
    require(isinstance(versions, list) and len(versions) <= 100, 'Invalid package version response.')
    # Only report matching alpha metadata; an empty first page is not proof of absence.
    facts['matching_versions_first_page'] = [
        {key: version.get(key) for key in ('name', 'created_at', 'updated_at')}
        for version in versions
        if VERSION in (((version.get('metadata') or {}).get('container') or {}).get('tags') or [])]
    return facts


def registry_facts(registry, inspect_manifest):
    digest = registry.version_digest(VERSION)
    if digest is None:
        return {'registry_tag_lookup': 'authenticated_404'}
    require(DIGEST.fullmatch(digest), 'Invalid registry digest.')
    manifest = inspect_manifest(IMAGE + '@' + digest)
    config = (manifest.get('config') or {}).get('digest')
    require(isinstance(config, str) and DIGEST.fullmatch(config), 'Unknown manifest config digest.')
    return {'registry_tag_lookup': 'found', 'digest': digest, 'config_digest': config,
            'expected_tested_image_id': EXPECTED_IMAGE, 'matches_run_40197637059_tested_image': config == EXPECTED_IMAGE}


def main():
    api = GitHub(os.environ.get('GH_TOKEN'))
    qualify_inspection(api, event_context())

    def report(phase, facts):
        text = json.dumps({'phase': phase, **facts}, sort_keys=True)
        print(text, flush=True)
        with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a', encoding='utf-8') as stream:
            stream.write('```json\n' + text + '\n```\n')

    report('package_metadata', metadata_facts(api))

    def inspect_manifest(ref):
        try:
            result = subprocess.run(['docker', 'buildx', 'imagetools', 'inspect', '--raw', ref],
                                    capture_output=True, text=True, check=True, timeout=30)
            require(len(result.stdout) <= 4 * 1024**2, 'Manifest exceeds inspection budget.')
            return json.loads(result.stdout)
        except (subprocess.SubprocessError, OSError, ValueError):
            raise PolicyError('Read-only manifest inspection failed; registry write state remains unknown.') from None

    try:
        report('registry_version', registry_facts(Registry(os.environ.get('GH_TOKEN'), os.environ.get('GITHUB_ACTOR')), inspect_manifest))
    except PolicyError as error:
        report('registry_version', {'state': 'unknown', 'reason': str(error)})
        raise


if __name__ == '__main__':
    try:
        main()
    except PolicyError as error:
        raise SystemExit(str(error)) from None
