"""Fail-closed source, CI and artifact policy for the container workflow.

No API mutations, credentials in build arguments, or execution of image code here.
"""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import urllib.error
import urllib.request

REPOSITORY = 'freesialuo/CalRelay'
IMAGE = 'ghcr.io/freesialuo/calrelay'
PREFIX = f'repos/{REPOSITORY}'
SHA = re.compile(r'[0-9a-f]{40}')
DIGEST = re.compile(r'sha256:[0-9a-f]{64}')
VERSION = re.compile(r'v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?', re.ASCII)
MAX_ARCHIVE = 2 * 1024**3


class PolicyError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise PolicyError(message)


@dataclass(frozen=True)
class Version:
    tag: str
    core: tuple
    prerelease: bool


def parse_version(tag):
    require(isinstance(tag, str) and len(tag) <= 128, 'Invalid version tag.')
    match = VERSION.fullmatch(tag)
    require(match is not None, 'Expected vMAJOR.MINOR.PATCH[-prerelease], without build metadata.')
    suffix = match[4]
    if suffix:
        require(all(not p.isdigit() or p == '0' or not p.startswith('0')
                    for p in suffix.split('.')), 'Prerelease numeric identifiers cannot have leading zeros.')
    return Version(tag, tuple(int(match[i]) for i in (1, 2, 3)), suffix is not None)


def positive(value):
    return type(value) is int and value > 0


def check_run(run, sha, workflow):
    require(all(positive(run.get(k)) for k in ('id', 'run_number', 'run_attempt')), 'Invalid CI identity.')
    require(run.get('workflow_id') == workflow and run.get('head_sha') == sha
            and run.get('head_branch') == 'main' and run.get('event') == 'push'
            and (run.get('repository') or {}).get('full_name') == REPOSITORY
            and (run.get('head_repository') or {}).get('full_name') == REPOSITORY,
            'CI must be an exact-SHA main push from this repository and CI workflow.')
    require(run.get('status') == 'completed' and run.get('conclusion') == 'success',
            'The latest eligible CI run is not completed successfully.')


def quality_gate(api, sha):
    workflow = api.get(PREFIX + '/actions/workflows/ci.yml')
    require(workflow.get('state') == 'active' and workflow.get('path') == '.github/workflows/ci.yml'
            and positive(workflow.get('id')), 'The CI workflow identity is invalid.')
    response = api.get(PREFIX + f'/actions/workflows/{workflow["id"]}/runs?event=push&branch=main&head_sha={sha}&per_page=100')
    runs = response.get('workflow_runs') or []
    require(runs and response.get('total_count', 0) <= 100, 'Missing or excessive eligible CI runs.')
    require(all(positive(r.get('run_number')) and positive(r.get('run_attempt')) for r in runs),
            'Invalid CI run metadata.')
    run = max(runs, key=lambda r: (r['run_number'], r['run_attempt']))
    check_run(run, sha, workflow['id'])
    response = api.get(PREFIX + f'/actions/runs/{run["id"]}/jobs?filter=latest&per_page=100')
    require(response.get('total_count', 0) <= 100, 'Excessive CI jobs.')
    jobs = [j for j in response.get('jobs', []) if j.get('name') == 'quality-gate']
    require(len(jobs) == 1 and jobs[0].get('status') == 'completed'
            and jobs[0].get('conclusion') == 'success', 'Missing or unsuccessful quality-gate job.')
    current = api.get(PREFIX + f'/actions/runs/{run["id"]}')
    check_run(current, sha, workflow['id'])
    require(current['run_attempt'] == run['run_attempt'], 'CI was rerun during validation.')
    return {'id': run['id'], 'attempt': run['run_attempt']}


def qualify_source(api, tag):
    repo = api.get(PREFIX)
    require(repo.get('private') is True and repo.get('default_branch') == 'main',
            'Expected the existing private repository and main branch; settings were not changed.')
    record = api.get(PREFIX + '/git/ref/tags/' + tag)
    require(record.get('ref') == 'refs/tags/' + tag, 'Tag identity mismatch.')
    obj, seen = record.get('object') or {}, set()
    for _ in range(6):
        value = obj.get('sha', '')
        require(isinstance(value, str) and SHA.fullmatch(value) and value not in seen, 'Invalid or cyclic tag object.')
        if obj.get('type') == 'commit':
            break
        require(obj.get('type') == 'tag', 'Tag must point to a commit.')
        seen.add(value)
        obj = api.get(PREFIX + '/git/tags/' + value).get('object') or {}
    else:
        raise PolicyError('Annotated tag chain is too deep.')
    compare = api.get(PREFIX + f'/compare/{value}...main')
    require(compare.get('status') in ('ahead', 'identical')
            and (compare.get('merge_base_commit') or {}).get('sha') == value,
            'Tagged code is not an ancestor of main.')
    return value, quality_gate(api, value)


def source_plan(event_name, event, ref, sha, workflow_sha, api):
    require((event.get('repository') or {}).get('full_name') == REPOSITORY, 'Untrusted repository event.')
    require(isinstance(sha, str) and SHA.fullmatch(sha)
            and isinstance(workflow_sha, str) and SHA.fullmatch(workflow_sha), 'Invalid workflow/source SHA.')
    result = {'schema': 1, 'repository': REPOSITORY, 'image': IMAGE,
              'workflow_sha': workflow_sha, 'sha': sha, 'publish': False,
              'version': '', 'stable': False, 'ci_run': 0, 'ci_attempt': 0}
    if event_name == 'pull_request':
        require(positive(event.get('number')) and ref == f'refs/pull/{event["number"]}/merge', 'Invalid PR context.')
        result['version'] = f'pr-{event["number"]}-{sha[:12]}'
        return result
    if event_name == 'release':
        release = event.get('release') or {}
        require(event.get('action') == 'published' and positive(release.get('id'))
                and release.get('draft') is False, 'Only published, non-draft releases qualify.')
        version = parse_version(release.get('tag_name'))
        require(ref == 'refs/tags/' + version.tag, 'Release ref does not match its version.')
        current = api.get(PREFIX + f'/releases/{release["id"]}')
        require(current.get('id') == release['id'] and current.get('tag_name') == version.tag
                and current.get('draft') is False and current.get('published_at')
                and current.get('prerelease') is version.prerelease
                and release.get('prerelease') is version.prerelease, 'Release state/version flags changed or disagree.')
        publish = True
    elif event_name == 'workflow_dispatch':
        require(ref == 'refs/heads/main', 'Manual image workflow must run from main.')
        inputs = event.get('inputs') or {}
        version = parse_version(inputs.get('version'))
        flag = inputs.get('publish', 'false')
        require(type(flag) is bool or type(flag) is str and flag in ('true', 'false'), 'Invalid publish input.')
        publish = flag is True or flag == 'true'
    else:
        raise PolicyError('Unsupported event; no workflow_run or pull_request_target promotion.')
    target, ci = qualify_source(api, version.tag)
    if event_name == 'release':
        require(target == sha, 'The release tag moved from the event SHA.')
    result.update(sha=target, version=version.tag, stable=not version.prerelease,
                  publish=publish, ci_run=ci['id'], ci_attempt=ci['attempt'])
    return result


def promote_latest(version, versions):
    if version.prerelease:
        return False
    current = versions.get('latest')
    if current is None:
        return True
    stable = []
    for tag in current['tags']:
        try:
            parsed = parse_version(tag)
        except PolicyError:
            continue
        if not parsed.prerelease:
            stable.append(parsed)
    require(len(stable) == 1, 'Existing latest must have exactly one canonical stable version; no guessing.')
    return version.core > stable[0].core


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class GitHub:
    def __init__(self, token):
        require(bool(token), 'GITHUB_TOKEN is required for trusted API reads.')
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect())

    def get(self, path, missing=False):
        require(path.startswith((PREFIX, 'users/freesialuo/packages/')), 'Unexpected API path.')
        req = urllib.request.Request('https://api.github.com/' + path, headers={
            'Authorization': 'Bearer ' + self.token, 'Accept': 'application/vnd.github+json',
            'X-GitHub-Api-Version': '2022-11-28'})
        try:
            with self.opener.open(req, timeout=15) as response:
                data = response.read(4 * 1024**2 + 1)
            require(len(data) <= 4 * 1024**2, 'API response exceeds budget.')
            return json.loads(data)
        except urllib.error.HTTPError as error:
            if error.code == 404 and missing:
                return None
            raise PolicyError(f'GitHub GET failed (HTTP {error.code}); no credential/permission fallback.') from None
        except (OSError, json.JSONDecodeError):
            raise PolicyError('GitHub GET failed; publication remains blocked.') from None


def file_hash(path, budget):
    require(not path.is_symlink() and stat.S_ISREG(path.stat().st_mode)
            and 0 < path.stat().st_size <= budget, 'Invalid artifact file or size.')
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def inspect_image(record, plan):
    require(DIGEST.fullmatch(record.get('Id', '')) and record.get('Os') == 'linux'
            and record.get('Architecture') == 'amd64', 'Expected a linux/amd64 image.')
    labels = (record.get('Config') or {}).get('Labels') or {}
    require(labels.get('org.opencontainers.image.revision') == plan['sha']
            and labels.get('org.opencontainers.image.version') == plan['version']
            and labels.get('org.opencontainers.image.source') == 'https://github.com/' + REPOSITORY,
            'Image labels do not bind this tested image to the source/version.')
    return record['Id']


def seal_bundle(directory, plan, record, run_id, attempt):
    require(positive(run_id) and positive(attempt), 'Invalid artifact run identity.')
    image_id = inspect_image(record, plan)
    directory = Path(directory)
    archive_hash = file_hash(directory / 'image.tar', MAX_ARCHIVE)
    manifest = {k: plan[k] for k in ('schema', 'repository', 'image', 'version', 'sha', 'workflow_sha')}
    manifest.update(run_id=run_id, run_attempt=attempt, image_id=image_id, archive_sha256=archive_hash)
    (directory / 'manifest.json').write_text(json.dumps(manifest, sort_keys=True) + '\n', encoding='utf-8')
    return {'image_id': image_id, 'archive_sha256': archive_hash,
            'manifest_sha256': file_hash(directory / 'manifest.json', 65536)}


def verify_bundle(directory, plan, expected, run_id, attempt):
    directory = Path(directory)
    require(not directory.is_symlink() and directory.is_dir()
            and {p.name for p in directory.iterdir()} == {'image.tar', 'manifest.json'}, 'Unexpected artifact layout.')
    require(DIGEST.fullmatch(expected.get('image_id', ''))
            and all(re.fullmatch(r'[0-9a-f]{64}', expected.get(k, ''))
                    for k in ('archive_sha256', 'manifest_sha256')), 'Invalid build outputs.')
    require(file_hash(directory / 'manifest.json', 65536) == expected['manifest_sha256'], 'Artifact manifest changed.')
    manifest = json.loads((directory / 'manifest.json').read_text(encoding='utf-8'))
    wanted = {k: plan[k] for k in ('schema', 'repository', 'image', 'version', 'sha', 'workflow_sha')}
    wanted.update(run_id=run_id, run_attempt=attempt, image_id=expected['image_id'],
                  archive_sha256=expected['archive_sha256'])
    require(manifest == wanted and all(type(manifest[k]) is type(v) for k, v in wanted.items()),
            'Artifact source, run, attempt or tested image mismatch.')
    require(file_hash(directory / 'image.tar', MAX_ARCHIVE) == expected['archive_sha256'], 'Image archive changed.')
    return manifest


PACKAGE = 'users/freesialuo/packages/container/calrelay'


def package_state(api):
    package = api.get(PACKAGE, missing=True)
    if package is None:
        return {}  # GitHub's documented default for a new package is private.
    require(package.get('visibility') == 'private'
            and (package.get('repository') or {}).get('full_name') == REPOSITORY,
            'Existing package must remain private and associated with this repository; no settings changed.')
    result = {}
    for page in range(1, 21):
        versions = api.get(PACKAGE + f'/versions?per_page=100&page={page}')
        require(isinstance(versions, list) and len(versions) <= 100, 'Invalid package version page.')
        for version in versions:
            digest = version.get('name', '')
            tags = ((version.get('metadata') or {}).get('container') or {}).get('tags')
            require(DIGEST.fullmatch(digest) and isinstance(tags, list)
                    and all(isinstance(t, str) and re.fullmatch(r'[\w][\w.-]{0,127}', t, re.ASCII) for t in tags),
                    'Invalid container version metadata.')
            for tag in tags:
                require(tag not in result or result[tag]['digest'] == digest, 'Ambiguous registry tag state.')
                result[tag] = {'digest': digest, 'tags': tags}
        if len(versions) < 100:
            return result
    raise PolicyError('Package history exceeds the publication budget; no overwrite attempted.')


def verify_remote(docker, digest, expected):
    ref = IMAGE + '@' + digest
    raw = json.loads(docker('buildx', 'imagetools', 'inspect', '--raw', ref))
    require(raw.get('mediaType') in ('application/vnd.docker.distribution.manifest.v2+json',
                                   'application/vnd.oci.image.manifest.v1+json')
            and (raw.get('config') or {}).get('digest') == expected,
            'Existing version is not the tested single-platform image; version tags are immutable.')
    docker('pull', '--platform=linux/amd64', ref)
    images = json.loads(docker('image', 'inspect', ref))
    require(len(images) == 1 and images[0].get('Id') == expected
            and images[0].get('Os') == 'linux' and images[0].get('Architecture') == 'amd64',
            'Published image contents/platform do not match the tested image.')


def publish_bundle(api, docker, context, directory, expected, run_id, attempt, pause):
    plan = source_plan(api=api, **context)
    require(plan['publish'], 'This event cannot publish packages.')
    manifest = verify_bundle(directory, plan, expected, run_id, attempt)
    candidate = 'calrelay-candidate:' + plan['sha']
    docker('load', '--input', str(Path(directory) / 'image.tar'))
    images = json.loads(docker('image', 'inspect', candidate))
    require(len(images) == 1 and inspect_image(images[0], plan) == manifest['image_id'], 'Loaded image mismatch.')
    versions = package_state(api)
    version = plan['version']
    if version not in versions:
        docker('tag', candidate, IMAGE + ':' + version)
        docker('push', IMAGE + ':' + version)
        # Metadata may lag the registry. Failure here leaves latest untouched.
        for retry in range(6):
            versions = package_state(api)
            if version in versions:
                break
            if retry < 5:
                pause(2)
        require(version in versions, 'Version push could not be confirmed; latest was not changed.')
    digest = versions[version]['digest']
    verify_remote(docker, digest, manifest['image_id'])
    # Re-read source, tag, latest CI and package state immediately before final promotion.
    current = source_plan(api=api, **context)
    require(all(current[k] == plan[k] for k in ('sha', 'version', 'workflow_sha', 'publish')),
            'Source or publication state changed during image transfer.')
    versions = package_state(api)
    require(versions.get(version, {}).get('digest') == digest, 'Version tag changed during publication.')
    latest = versions.get('latest')
    if latest and version in latest['tags']:
        require(latest['digest'] == digest, 'Existing latest/version association is inconsistent.')
    promoted = promote_latest(parse_version(version), versions)
    if promoted:
        metadata = Path(directory) / 'promotion.json'
        docker('buildx', 'imagetools', 'create', '--prefer-index=false', '--metadata-file', str(metadata),
               '--tag', IMAGE + ':latest', IMAGE + '@' + digest)
        result = json.loads(metadata.read_text(encoding='utf-8'))
        require((result.get('containerimage.descriptor') or {}).get('digest') == digest,
                'Latest promotion result is uncertain; inspect the registry before retrying.')
    return {**current, 'digest': digest, 'latest_promoted': promoted, 'image_id': manifest['image_id']}


def event_context():
    return {'event_name': os.environ['GITHUB_EVENT_NAME'],
            'event': json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text(encoding='utf-8')),
            'ref': os.environ['GITHUB_REF'], 'sha': os.environ['GITHUB_SHA'],
            'workflow_sha': os.environ['GITHUB_WORKFLOW_SHA']}


def outputs(values):
    target = os.environ.get('GITHUB_OUTPUT')
    if target:
        with open(target, 'a', encoding='utf-8') as stream:
            for key, value in values.items():
                text = str(value).lower() if type(value) is bool else str(value)
                require('\n' not in text and '\r' not in text, 'Multiline workflow output rejected.')
                stream.write(f'{key}={text}\n')


def cli():
    import argparse
    import subprocess
    import time
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('plan', 'seal', 'publish'))
    parser.add_argument('--bundle', type=Path)
    args = parser.parse_args()
    if args.command == 'plan':
        context = event_context()
        api = None if context['event_name'] == 'pull_request' else GitHub(os.environ.get('GH_TOKEN'))
        result = source_plan(api=api, **context)
        outputs({**result, 'plan': json.dumps(result, separators=(',', ':'))})
        return
    require(args.bundle is not None, 'A fixed artifact directory is required.')
    run_id, attempt = int(os.environ['GITHUB_RUN_ID']), int(os.environ['GITHUB_RUN_ATTEMPT'])

    def docker(*command):
        try:
            return subprocess.run(['docker', *command], check=True, capture_output=True, text=True).stdout
        except subprocess.CalledProcessError:
            raise PolicyError('Docker operation failed; inspect publication state before retrying.') from None

    if args.command == 'seal':
        plan = json.loads(os.environ['IMAGE_PLAN'])
        record = json.loads(docker('image', 'inspect', 'calrelay-candidate:' + plan['sha']))[0]
        outputs(seal_bundle(args.bundle, plan, record, run_id, attempt))
        return
    expected = {key: os.environ['EXPECTED_' + key.upper()]
                for key in ('image_id', 'archive_sha256', 'manifest_sha256')}
    result = publish_bundle(GitHub(os.environ.get('GH_TOKEN')), docker, event_context(), args.bundle,
                            expected, run_id, attempt, time.sleep)
    note = (f'## Container release {result["version"]}\n\n'
            f'- Image: `{IMAGE}@{result["digest"]}`\n'
            f'- Platform: `linux/amd64`\n- Source: `{result["sha"]}`\n'
            f'- CI: run {result["ci_run"]}, attempt {result["ci_attempt"]}\n'
            f'- Image ID: `{result["image_id"]}`\n'
            f'- Latest promoted: `{result["latest_promoted"]}`\n\n'
            f'Pull by digest; this workflow does not deploy a server.\n')
    (args.bundle / 'release-notes.md').write_text(note, encoding='utf-8')
    with open(os.environ['GITHUB_STEP_SUMMARY'], 'a', encoding='utf-8') as summary:
        summary.write(note)
    outputs({'digest': result['digest']})


if __name__ == '__main__':
    try:
        cli()
    except (PolicyError, OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        # Only policy messages are safe; never dump request data, tokens or a traceback.
        message = str(error) if isinstance(error, PolicyError) else 'Invalid release input or I/O; no fallback credentials used.'
        raise SystemExit(message) from None
