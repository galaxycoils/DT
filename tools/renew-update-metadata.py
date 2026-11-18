#!/usr/bin/env python3
"""Renew the signed metadata for an existing authenticated DT release."""
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
verify = runpy.run_path(str(HERE / 'verify-release-evidence.py'))['verify']
sign = runpy.run_path(str(HERE / 'make-release-manifest.py'))['sign_bytes']
release = runpy.run_path(str(HERE / 'sign-release.py'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--sequence', required=True, type=int)
    parser.add_argument('--release-dir', required=True, type=pathlib.Path)
    parser.add_argument('--metadata-dir', required=True, type=pathlib.Path)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', args.version):
        parser.error('Expected a stable version in X.Y.Z format')
    if not re.fullmatch(r'[0-9a-f]{40}', args.commit) or args.sequence < 1:
        parser.error('Expected a full commit SHA and a positive sequence')

    public = os.environ['GM_RELEASE_PUBLIC_KEY_HEX'].strip().lower()
    raw = (args.release_dir / 'dt-release.json').read_bytes()
    verify(raw, (args.release_dir / 'dt-release.sig').read_text(), public)
    manifest = json.loads(raw)
    expected = {(release['asset_filename'](args.version, os_name, arch), os_name, arch)
                for os_name, arch in release['TARGETS']}
    actual = {(asset['name'], asset['os'], asset['architecture']) for asset in manifest['assets']}
    if manifest['schema'] != 1 or manifest['version'] != args.version or actual != expected or len(manifest['assets']) != len(expected):
        parser.error('The signed release manifest must list the four exact target installers')

    for asset in manifest['assets']:
        path = args.release_dir / asset['name']
        if path.is_symlink() or path.stat().st_size != asset['size_bytes']:
            parser.error(f'Package size mismatch: {path.name}')
        with path.open('rb') as package:
            if hashlib.file_digest(package, 'sha256').hexdigest() != asset['sha256']:
                parser.error(f'Package digest mismatch: {path.name}')

    prior_raw = (args.metadata_dir / 'dt-update-metadata.json').read_bytes()
    verify(prior_raw, (args.metadata_dir / 'dt-update-metadata.sig').read_text(), public)
    prior = json.loads(prior_raw)
    if (prior['repo'], prior['version'], prior['commit']) != (args.repo, args.version, args.commit):
        parser.error('Renewals must target the existing signed release and source commit')
    if prior['assets'] != manifest['assets'] or args.sequence <= prior['sequence']:
        parser.error('Renewals must preserve assets and strictly increase the sequence')
    if args.version in prior['revoked_versions']:
        parser.error('A revoked release cannot be renewed')

    now = datetime.datetime.now(datetime.timezone.utc)
    prior.update(sequence=args.sequence, published_at=now.isoformat(),
                 expires_at=(now + datetime.timedelta(days=30)).isoformat())
    output = (json.dumps(prior, sort_keys=True, separators=(',', ':')) + '\n').encode()
    with tempfile.TemporaryDirectory(prefix='dt-renew-') as folder:
        key = pathlib.Path(folder) / 'publisher.pem'
        key.touch(mode=0o600)
        key.write_text(os.environ['GM_RELEASE_SIGNING_KEY_PEM'])
        derived = subprocess.check_output(['openssl', 'pkey', '-in', str(key), '-pubout', '-outform', 'DER'])
        if derived.hex() != '302a300506032b6570032100' + public:
            parser.error('Signing key does not match the pinned publisher key')
        sign(output, key, args.metadata_dir / 'dt-update-metadata.json', args.metadata_dir / 'dt-update-metadata.sig')


if __name__ == '__main__':
    main()
