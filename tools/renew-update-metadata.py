#!/usr/bin/env python3
"""Promote or renew the signed metadata for an existing authenticated DT release."""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import re
import runpy
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
EVIDENCE = runpy.run_path(str(HERE / 'verify-release-evidence.py'))
MANIFEST = runpy.run_path(str(HERE / 'make-release-manifest.py'))
verify = EVIDENCE['verify']
sign = MANIFEST['sign_bytes']
find_openssl = MANIFEST['find_openssl']
load_inventory = MANIFEST['load_inventory']
expected_asset_name = MANIFEST['expected_asset_name']


def verify_manifest_and_assets(release_dir: pathlib.Path, manifest_name: str, sig_name: str, version: str, expected_targets: set, public: str, parser) -> dict:
    raw = (release_dir / manifest_name).read_bytes()
    verify(raw, (release_dir / sig_name).read_text(), public)
    manifest = json.loads(raw)
    expected = {(expected_asset_name(version, os_name, arch), os_name, arch) for os_name, arch in expected_targets}
    actual = {(asset['name'], asset['os'], asset['architecture']) for asset in manifest['assets']}
    if manifest['schema'] != 1 or manifest['version'] != version or actual != expected or len(manifest['assets']) != len(expected):
        parser.error(f'{manifest_name} must list the exact expected target installers')
    for asset in manifest['assets']:
        path = release_dir / asset['name']
        if path.is_symlink() or path.stat().st_size != asset['size_bytes']:
            parser.error(f'Package size mismatch: {path.name}')
        with path.open('rb') as package:
            if hashlib.file_digest(package, 'sha256').hexdigest() != asset['sha256']:
                parser.error(f'Package digest mismatch: {path.name}')
    return manifest


def update_metadata_pair(
    *,
    meta_path: pathlib.Path,
    sig_path: pathlib.Path,
    manifest: dict,
    repo: str,
    version: str,
    commit: str,
    sequence: int,
    promote_first: bool,
    public: str,
    key: pathlib.Path,
    parser,
):
    now = datetime.datetime.now(datetime.timezone.utc)
    expires = (now + datetime.timedelta(days=30)).isoformat().replace('+00:00', 'Z')
    published_at = now.isoformat().replace('+00:00', 'Z')
    revoked_versions: list[str] = []

    if meta_path.is_file() and sig_path.is_file():
        prior_raw = meta_path.read_bytes()
        verify(prior_raw, sig_path.read_text(), public)
        prior = json.loads(prior_raw)
        if prior['repo'] != repo or sequence <= prior['sequence']:
            parser.error(f'Metadata sequence ({sequence}) must strictly exceed prior ({prior["sequence"]}) for repo {repo}')
        if version in prior.get('revoked_versions', []):
            parser.error('A revoked release cannot be promoted or renewed')
        revoked_versions = list(prior.get('revoked_versions', []))
        if not promote_first:
            if (prior['version'], prior['commit']) != (version, commit):
                parser.error('Renewals must target the existing signed release and source commit (use --promote-first for a new version)')
            if prior['assets'] != manifest['assets']:
                parser.error('Renewals must preserve exact asset hashes')
    elif not promote_first:
        parser.error(f'Missing prior metadata {meta_path}; pass --promote-first for initial signed promotion')

    payload = dict(
        schema=1,
        sequence=sequence,
        repo=repo,
        version=version,
        commit=commit,
        published_at=published_at,
        expires_at=expires,
        revoked_versions=revoked_versions,
        assets=manifest['assets'],
    )
    output = (json.dumps(payload, sort_keys=True, separators=(',', ':')) + '\n').encode()
    sign(output, key, meta_path, sig_path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--sequence', required=True, type=int)
    parser.add_argument('--release-dir', required=True, type=pathlib.Path)
    parser.add_argument('--metadata-dir', required=True, type=pathlib.Path)
    parser.add_argument('--promote-first', action='store_true', help='Promote a newly signed release version onto update-metadata')
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', args.version):
        parser.error('Expected a stable version in X.Y.Z format')
    if not re.fullmatch(r'[0-9a-f]{40}', args.commit) or args.sequence < 1:
        parser.error('Expected a full commit SHA and a positive sequence')

    inventory = load_inventory()
    historical = {(t['os'], t['architecture']) for t in inventory['historical_v1_targets']}
    all_targets = {(t['os'], t['architecture']) for t in inventory['targets']}

    public = os.environ['GM_RELEASE_PUBLIC_KEY_HEX'].strip().lower()
    manifest_v1 = verify_manifest_and_assets(args.release_dir, 'dt-release.json', 'dt-release.sig', args.version, historical, public, parser)
    has_v2 = (args.release_dir / 'dt-release-v2.json').is_file()
    manifest_v2 = verify_manifest_and_assets(args.release_dir, 'dt-release-v2.json', 'dt-release-v2.sig', args.version, all_targets, public, parser) if has_v2 else None

    openssl = find_openssl()
    args.metadata_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='dt-renew-') as folder:
        key = pathlib.Path(folder) / 'publisher.pem'
        key.touch(mode=0o600)
        key.write_text(os.environ['GM_RELEASE_SIGNING_KEY_PEM'])
        derived = subprocess.check_output([openssl, 'pkey', '-in', str(key), '-pubout', '-outform', 'DER'])
        if derived.hex() != '302a300506032b6570032100' + public:
            parser.error('Signing key does not match the pinned publisher key')
        update_metadata_pair(
            meta_path=args.metadata_dir / 'dt-update-metadata.json',
            sig_path=args.metadata_dir / 'dt-update-metadata.sig',
            manifest=manifest_v1,
            repo=args.repo, version=args.version, commit=args.commit,
            sequence=args.sequence, promote_first=args.promote_first,
            public=public, key=key, parser=parser,
        )
        if manifest_v2 is not None:
            update_metadata_pair(
                meta_path=args.metadata_dir / 'dt-update-metadata-v2.json',
                sig_path=args.metadata_dir / 'dt-update-metadata-v2.sig',
                manifest=manifest_v2,
                repo=args.repo, version=args.version, commit=args.commit,
                sequence=args.sequence, promote_first=args.promote_first,
                public=public, key=key, parser=parser,
            )


if __name__ == '__main__':
    main()
