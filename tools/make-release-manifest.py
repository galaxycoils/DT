#!/usr/bin/env python3
"""Build Ed25519-authenticated DT release manifest and renewable update metadata."""
import argparse
import datetime
import hashlib
import json
import pathlib
import subprocess
import tempfile


def expected_asset_name(version: str, os_name: str, architecture: str) -> str:
    if os_name == 'macos':
        return f'DT-{version}-macos-{architecture}.dmg'
    if os_name == 'windows':
        return f'DT-{version}-windows-x86_64-setup.exe'
    return f'dt_{version}_amd64.deb'


def sign_bytes(raw: bytes, private_key: pathlib.Path, out_json: pathlib.Path, out_sig: pathlib.Path) -> str:
    with tempfile.TemporaryDirectory(prefix='dt-sign-') as folder:
        folder = pathlib.Path(folder)
        manifest, signature = folder / 'manifest', folder / 'signature'
        manifest.write_bytes(raw)
        subprocess.run(['openssl', 'pkeyutl', '-sign', '-rawin', '-inkey', str(private_key), '-in', str(manifest), '-out', str(signature)], check=True)
        public = subprocess.check_output(['openssl', 'pkey', '-in', str(private_key), '-pubout', '-outform', 'DER'])
        if not public.startswith(bytes.fromhex('302a300506032b6570032100')) or len(public) != 44 or len(signature.read_bytes()) != 64:
            raise ValueError('The release key must be Ed25519')
        out_json.write_bytes(raw)
        out_sig.write_text(signature.read_bytes().hex() + '\n')
        return public[-32:].hex()


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
    assets, identities = [], set()
    for spec in args.asset:
        os_name, architecture, filename = spec.split(':', 2)
        if os_name not in ('linux', 'macos', 'windows') or architecture not in ('x86_64', 'aarch64'):
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
    raw = (json.dumps(dict(schema=1, version=args.version, expires_at=args.expires, assets=assets), sort_keys=True, separators=(',', ':')) + '\n').encode()
    pub_hex = sign_bytes(raw, args.private_key, args.output_dir / 'dt-release.json', args.output_dir / 'dt-release.sig')

    renewable = (json.dumps(dict(
        schema=1,
        sequence=args.sequence,
        repo=args.repo,
        version=args.version,
        commit=args.commit,
        published_at=datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z'),
        expires_at=args.expires,
        revoked_versions=args.revoked_version,
        assets=assets,
    ), sort_keys=True, separators=(',', ':')) + '\n').encode()
    sign_bytes(renewable, args.private_key, args.output_dir / 'dt-update-metadata.json', args.output_dir / 'dt-update-metadata.sig')
    print('Publisher public key (pin independently in the release build): ' + pub_hex)


if __name__ == '__main__':
    main()
