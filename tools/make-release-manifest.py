#!/usr/bin/env python3
"""Build Ed25519-authenticated DT release manifests (v1-compatible + v2 full inventory) and renewable update metadata."""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
INVENTORY_PATH = ROOT / 'docs/release/release-inventory.json'


def load_inventory():
    return json.loads(INVENTORY_PATH.read_text())


def find_openssl() -> str:
    candidates = [
        os.environ.get('DT_OPENSSL'),
        '/opt/homebrew/opt/openssl@3/bin/openssl',
        '/usr/local/opt/openssl@3/bin/openssl',
        shutil.which('openssl'),
    ]
    for candidate in candidates:
        if not candidate:
            continue
        if os.path.isabs(candidate) and not os.path.exists(candidate):
            continue
        with tempfile.TemporaryDirectory() as folder:
            probe = pathlib.Path(folder) / 'probe.pem'
            try:
                res = subprocess.run([candidate, 'genpkey', '-algorithm', 'ED25519', '-out', str(probe)], capture_output=True)
            except OSError:
                continue
            if res.returncode == 0:
                return candidate
    raise RuntimeError('OpenSSL with Ed25519 support is required (set DT_OPENSSL or install OpenSSL 3)')


def expected_asset_name(version: str, os_name: str, architecture: str) -> str:
    inventory = load_inventory()
    for target in inventory['targets']:
        if target['os'] == os_name and target['architecture'] == architecture:
            return target['asset_template'].format(version=version)
    raise ValueError(f'Unsupported OS/architecture: {os_name}/{architecture}')


def sign_bytes(raw: bytes, private_key: pathlib.Path, out_json: pathlib.Path, out_sig: pathlib.Path) -> str:
    openssl = find_openssl()
    with tempfile.TemporaryDirectory(prefix='dt-sign-') as folder:
        folder = pathlib.Path(folder)
        manifest, signature = folder / 'manifest', folder / 'signature'
        manifest.write_bytes(raw)
        subprocess.run([openssl, 'pkeyutl', '-sign', '-rawin', '-inkey', str(private_key), '-in', str(manifest), '-out', str(signature)], check=True)
        public = subprocess.check_output([openssl, 'pkey', '-in', str(private_key), '-pubout', '-outform', 'DER'])
        if not public.startswith(bytes.fromhex('302a300506032b6570032100')) or len(public) != 44 or len(signature.read_bytes()) != 64:
            raise ValueError('The release key must be Ed25519')
        out_json.write_bytes(raw)
        out_sig.write_text(signature.read_bytes().hex() + '\n')
        return public[-32:].hex()


def write_manifest_pair(
    output_dir: pathlib.Path,
    manifest_name: str,
    manifest_sig_name: str,
    metadata_name: str,
    metadata_sig_name: str,
    *,
    version: str,
    expires: str,
    commit: str,
    sequence: int,
    repo: str,
    revoked_versions: list[str],
    assets: list[dict],
    private_key: pathlib.Path,
) -> str:
    raw = (json.dumps(dict(schema=1, version=version, expires_at=expires, assets=assets), sort_keys=True, separators=(',', ':')) + '\n').encode()
    pub_hex = sign_bytes(raw, private_key, output_dir / manifest_name, output_dir / manifest_sig_name)
    renewable = (json.dumps(dict(
        schema=1,
        sequence=sequence,
        repo=repo,
        version=version,
        commit=commit,
        published_at=datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z'),
        expires_at=expires,
        revoked_versions=revoked_versions,
        assets=assets,
    ), sort_keys=True, separators=(',', ':')) + '\n').encode()
    sign_bytes(renewable, private_key, output_dir / metadata_name, output_dir / metadata_sig_name)
    return pub_hex


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--expires', required=True, help='RFC3339 UTC expiry, e.g. 2027-01-01T00:00:00Z')
    parser.add_argument('--asset', action='append', required=True, help='os:architecture:/path/to/asset')
    parser.add_argument('--private-key', type=pathlib.Path, required=True, help='Offline Ed25519 PEM key')
    parser.add_argument('--output-dir', type=pathlib.Path, required=True)
    parser.add_argument('--commit', default='0' * 40)
    parser.add_argument('--sequence', type=int, default=1)
    parser.add_argument('--repo', default='galaxycoils/DT')
    parser.add_argument('--revoked-version', action='append', default=[])
    args = parser.parse_args()
    expires = datetime.datetime.fromisoformat(args.expires.replace('Z', '+00:00'))
    if expires.tzinfo is None or expires <= datetime.datetime.now(datetime.timezone.utc):
        parser.error('Expiry must be a future time with a timezone')
    inventory = load_inventory()
    historical = {(t['os'], t['architecture']) for t in inventory['historical_v1_targets']}
    all_targets = {(t['os'], t['architecture']) for t in inventory['targets']}

    assets, identities = [], set()
    for spec in args.asset:
        os_name, architecture, filename = spec.split(':', 2)
        if (os_name, architecture) not in all_targets:
            parser.error('Unsupported OS or architecture')
        path = pathlib.Path(filename)
        expected = expected_asset_name(args.version, os_name, architecture)
        if path.name != expected:
            parser.error(f'Asset must use canonical exact-target name: {expected}')
        identity = (os_name, architecture)
        if identity in identities or not path.is_file() or path.is_symlink():
            parser.error('Duplicate asset or asset is not a regular file')
        identities.add(identity)
        size = path.stat().st_size
        if not 0 < size <= 2 * 1024**3:
            parser.error('Asset size must be between 1 byte and 2 GiB')
        with path.open('rb') as source:
            hasher = hashlib.sha256()
            for chunk in iter(lambda: source.read(65536), b''):
                hasher.update(chunk)
            digest = hasher.hexdigest()
        assets.append(dict(name=path.name, os=os_name, architecture=architecture, size_bytes=size, sha256=digest))

    args.output_dir.mkdir(parents=True, exist_ok=True)
    v1_assets = [a for a in assets if (a['os'], a['architecture']) in historical]
    if v1_assets:
        pub_hex = write_manifest_pair(
            args.output_dir,
            'dt-release.json', 'dt-release.sig',
            'dt-update-metadata.json', 'dt-update-metadata.sig',
            version=args.version, expires=args.expires, commit=args.commit,
            sequence=args.sequence, repo=args.repo, revoked_versions=args.revoked_version,
            assets=v1_assets, private_key=args.private_key,
        )
    else:
        pub_hex = ''
    pub_hex_v2 = write_manifest_pair(
        args.output_dir,
        'dt-release-v2.json', 'dt-release-v2.sig',
        'dt-update-metadata-v2.json', 'dt-update-metadata-v2.sig',
        version=args.version, expires=args.expires, commit=args.commit,
        sequence=args.sequence, repo=args.repo, revoked_versions=args.revoked_version,
        assets=assets, private_key=args.private_key,
    )
    print('Publisher public key (pin independently in the release build): ' + (pub_hex_v2 or pub_hex))


if __name__ == '__main__':
    main()
