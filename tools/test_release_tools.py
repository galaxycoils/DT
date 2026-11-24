"""Protocol/tool regressions. Synthetic inputs here do not certify a release."""
import sys
if sys.version_info < (3, 11):
    raise SystemExit("Python 3.11+ is required to run DT release tool checks (e.g., python3.13 tools/test_release_tools.py)")

import copy
import datetime
import hashlib
import json
import os
import pathlib
import runpy
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parent.parent
EVIDENCE = runpy.run_path(str(ROOT / 'tools/verify-release-evidence.py'))
PACKAGE = runpy.run_path(str(ROOT / 'tools/package-release.py'))
MANIFEST = runpy.run_path(str(ROOT / 'tools/make-release-manifest.py'))


def build_raw_samples():
    return {
        'idle_tree_mib': [30.0] * 30,
        'ui_tree_mib': [100.0] * 30,
        'capture_ms': [1.0] * 30,
        'ax_ms': [10.0] * 30,
        'jpeg_ms': [5.0] * 30,
        'dispatch_ms': [1.0] * 30,
        'kill_ms': [10.0] * 30,
        'step_ms': [100.0] * 15 + [500.0] * 15,
    }


def make_synthetic_pe(machine: int) -> bytes:
    buf = bytearray(256)
    buf[0:2] = b'MZ'
    struct.pack_into('<I', buf, 0x3C, 0x80)
    buf[0x80:0x84] = b'PE\x00\x00'
    struct.pack_into('<H', buf, 0x84, machine)
    return bytes(buf)


def make_synthetic_elf(e_machine: int) -> bytes:
    buf = bytearray(64)
    buf[0:4] = b'\x7fELF'
    struct.pack_into('<H', buf, 18, e_machine)
    return bytes(buf)


class EvidenceTests(unittest.TestCase):
    def fixture(self, root):
        (root / 'log.txt').write_text('synthetic tool regression fixture, not native evidence\n')
        checksum = hashlib.sha256((root / 'log.txt').read_bytes()).hexdigest()
        catalog = json.loads((ROOT / 'docs/release/acceptance-tasks.json').read_text())
        tasks = [dict(id=item['id'], workflow=item['workflow'], success=True, unapproved_irreversible=False) for item in catalog]
        inventory = json.loads((ROOT / 'docs/release/coverage-inventory.json').read_text())
        coverage_rows = {row['id']: True for row in inventory['rows'] if row.get('required')}
        release_inv = json.loads((ROOT / 'docs/release/release-inventory.json').read_text())
        version = release_inv['version']
        metrics = dict(
            samples=30,
            installed_binary_hashes_verified=True,
            jpeg_decoded_verified=True,
            idle_tree_mib=30.0,
            ui_tree_mib=100.0,
            capture_p99_ms=1.0,
            ax_p95_ms=10.0,
            jpeg_p95_ms=5.0,
            dispatch_p95_ms=1.0,
            kill_ms=10.0,
            step_p50_ms=100.0,
            step_p95_ms=500.0,
            raw_samples=build_raw_samples(),
        )
        recorded = datetime.datetime.now(datetime.timezone.utc)
        soak = dict(started_at=(recorded - datetime.timedelta(days=7)).isoformat(),
                    ended_at=recorded.isoformat(), ratings=20, useful=16,
                    max_notices_per_day=3, quiet_hours=True, opt_out=True, budget=True)
        reports = [dict(os=os_name, architecture=arch, session=session, checks={key: True for key in EVIDENCE['CHECKS']},
                        runs=[dict(tasks=copy.deepcopy(tasks)) for _ in range(3)], measurements=copy.deepcopy(metrics), soak=soak,
                        artifacts=[dict(path='log.txt', sha256=checksum)]) for os_name, arch, session in EVIDENCE['TARGETS']]
        return dict(schema=1, commit='a' * 40, source='native', recorded_at=recorded.isoformat(),
                    coverage_rows=coverage_rows, targets=reports, candidate_hashes={
                        'installers': {t['asset_template'].format(version=version): checksum
                                       for t in release_inv['targets']},
                        'container': 'sha256:' + checksum,
                        'mobile': {'ios': checksum, 'android': checksum},
                        'components': {name: checksum for name in release_inv['components']},
                    })

    def test_required_targets_catalog_safety_and_metrics(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            data = self.fixture(root)
            version = json.loads((ROOT / 'docs/release/release-inventory.json').read_text())['version']
            EVIDENCE['validate'](data, 'a' * 40, root)
            mutations = [
                lambda d: d.update(source='fixture'),
                lambda d: d.pop('candidate_hashes'),
                lambda d: d['candidate_hashes']['installers'].pop(f'DT-{version}-windows-aarch64-setup.exe'),
                lambda d: d['candidate_hashes']['installers'].pop(f'dt_{version}_arm64.deb'),
                lambda d: d.update(commit='b' * 40),
                lambda d: d['targets'].pop(),
                lambda d: d.update(targets=[t for t in d['targets'] if t['session'] != 'wayland']),
                lambda d: d['targets'][0]['runs'].pop(),
                lambda d: d['targets'][0]['runs'][0]['tasks'][0].update(unapproved_irreversible=True),
                lambda d: d['targets'][0]['runs'][0]['tasks'][0].update(success=False),
                lambda d: d['targets'][0]['runs'][0]['tasks'][0].update(id='unknown-task'),
                lambda d: d['targets'][0]['checks'].update(vm_cli_boot=False),
                lambda d: d['targets'][0]['measurements'].update(jpeg_decoded_verified=False),
                lambda d: d['targets'][0]['measurements'].update(installed_binary_hashes_verified=False),
                lambda d: d['targets'][0]['measurements']['raw_samples']['jpeg_ms'].pop(),
                lambda d: d['targets'][0]['measurements'].update(kill_ms=150),
                lambda d: d['targets'][0]['soak'].update(useful=15),
                lambda d: d['targets'][0]['artifacts'][0].update(path='../outside'),
                lambda d: d['coverage_rows'].popitem(),
            ]
            for mutate in mutations:
                with self.subTest(mutate=mutate):
                    changed = copy.deepcopy(data)
                    mutate(changed)
                    with self.assertRaises(ValueError):
                        EVIDENCE['validate'](changed, 'a' * 40, root)
            (root / 'log.txt').write_text('changed')
            with self.assertRaises(ValueError):
                EVIDENCE['validate'](data, 'a' * 40, root)

    def test_signatures_v1_v2_manifests_and_renewal(self):
        openssl = MANIFEST['find_openssl']()
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            key = root / 'test-only.pem'
            subprocess.run([openssl, 'genpkey', '-algorithm', 'ED25519', '-out', str(key)], check=True, capture_output=True)
            public = subprocess.check_output([openssl, 'pkey', '-in', str(key), '-pubout', '-outform', 'DER'])[-32:].hex()
            raw = b'fixture receipt'
            source = root / 'receipt'
            source.write_bytes(raw)
            signature = root / 'receipt.sig'
            subprocess.run([openssl, 'pkeyutl', '-sign', '-rawin', '-inkey', str(key), '-in', str(source), '-out', str(signature)], check=True, capture_output=True)
            signature_hex = signature.read_bytes().hex()
            EVIDENCE['verify'](raw, signature_hex, public)
            with self.assertRaises(subprocess.CalledProcessError):
                EVIDENCE['verify'](b'changed', signature_hex, public)

            # Build all 6 target dummy assets
            release_inv = json.loads((ROOT / 'docs/release/release-inventory.json').read_text())
            version = release_inv['version']
            asset_args = []
            for t in release_inv['targets']:
                name = t['asset_template'].format(version=version)
                p = root / name
                p.write_bytes(f'synthetic-{name}'.encode())
                asset_args += ['--asset', f"{t['os']}:{t['architecture']}:{p}"]

            expires = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=1)).isoformat()
            commit = 'c' * 40
            command = [
                sys.executable, str(ROOT / 'tools/make-release-manifest.py'),
                '--version', version, '--expires', expires, '--commit', commit,
                '--sequence', '1',
                *asset_args,
                '--private-key', str(key), '--output-dir', str(root / 'signed'),
            ]
            subprocess.run(command, check=True, capture_output=True)

            # v1 manifest contains ONLY the 4 historical targets (safe for v1.0.0 updaters)
            v1_manifest_raw = (root / 'signed/dt-release.json').read_bytes()
            EVIDENCE['verify'](v1_manifest_raw, (root / 'signed/dt-release.sig').read_text(), public)
            v1_manifest = json.loads(v1_manifest_raw)
            self.assertEqual(len(v1_manifest['assets']), 4)
            self.assertFalse(any(a['architecture'] == 'aarch64' and a['os'] in ('windows', 'linux') for a in v1_manifest['assets']))

            # v2 manifest contains all 6 targets
            v2_manifest_raw = (root / 'signed/dt-release-v2.json').read_bytes()
            EVIDENCE['verify'](v2_manifest_raw, (root / 'signed/dt-release-v2.sig').read_text(), public)
            v2_manifest = json.loads(v2_manifest_raw)
            self.assertEqual(len(v2_manifest['assets']), 6)

            # First signed promotion + subsequent renewal
            for t in release_inv['targets']:
                name = t['asset_template'].format(version=version)
                (root / 'signed' / name).write_bytes((root / name).read_bytes())

            env = os.environ.copy()
            env['GM_RELEASE_PUBLIC_KEY_HEX'] = public
            env['GM_RELEASE_SIGNING_KEY_PEM'] = key.read_text()
            meta_dir = root / 'meta'
            subprocess.run([
                sys.executable, str(ROOT / 'tools/renew-update-metadata.py'),
                '--repo', 'galaxycoils/DT', '--version', version, '--commit', commit,
                '--sequence', '2', '--release-dir', str(root / 'signed'),
                '--metadata-dir', str(meta_dir), '--promote-first',
            ], env=env, check=True, capture_output=True)
            subprocess.run([
                sys.executable, str(ROOT / 'tools/renew-update-metadata.py'),
                '--repo', 'galaxycoils/DT', '--version', version, '--commit', commit,
                '--sequence', '3', '--release-dir', str(root / 'signed'),
                '--metadata-dir', str(meta_dir),
            ], env=env, check=True, capture_output=True)
            EVIDENCE['verify']((meta_dir / 'dt-update-metadata-v2.json').read_bytes(),
                               (meta_dir / 'dt-update-metadata-v2.sig').read_text(), public)

    def test_exact_platform_names_and_binary_architecture_checks(self):
        self.assertEqual(PACKAGE['archive_name']('1.0.1', 'macos', 'aarch64'), 'DT-1.0.1-macos-aarch64.dmg')
        self.assertEqual(PACKAGE['archive_name']('1.0.1', 'macos', 'x86_64'), 'DT-1.0.1-macos-x86_64.dmg')
        self.assertEqual(PACKAGE['archive_name']('1.0.1', 'windows', 'x86_64'), 'DT-1.0.1-windows-x86_64-setup.exe')
        self.assertEqual(PACKAGE['archive_name']('1.0.1', 'windows', 'aarch64'), 'DT-1.0.1-windows-aarch64-setup.exe')
        self.assertEqual(PACKAGE['archive_name']('1.0.1', 'linux', 'x86_64'), 'dt_1.0.1_amd64.deb')
        self.assertEqual(PACKAGE['archive_name']('1.0.1', 'linux', 'aarch64'), 'dt_1.0.1_arm64.deb')
        with self.assertRaises(ValueError):
            PACKAGE['archive_name']('1.0.1', 'linux', 'riscv64')
        with self.assertRaises(ValueError):
            PACKAGE['archive_name']('../bad', 'macos', 'aarch64')

        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            win_x64 = root / 'x64.exe'
            win_x64.write_bytes(make_synthetic_pe(0x8664))
            PACKAGE['verify_binary_architecture'](win_x64, 'windows', 'x86_64')
            with self.assertRaises(ValueError):
                PACKAGE['verify_binary_architecture'](win_x64, 'windows', 'aarch64')

            linux_arm = root / 'arm64'
            linux_arm.write_bytes(make_synthetic_elf(183))
            PACKAGE['verify_binary_architecture'](linux_arm, 'linux', 'aarch64')
            with self.assertRaises(ValueError):
                PACKAGE['verify_binary_architecture'](linux_arm, 'linux', 'x86_64')

            universal = root / 'universal'
            universal.write_bytes(bytes.fromhex('cafebabe00000002'))
            with patch('shutil.which', return_value=None), self.assertRaisesRegex(ValueError, 'lipo'):
                PACKAGE['verify_binary_architecture'](universal, 'macos', 'aarch64')

    def test_sign_release_rejects_dirty_checkout_and_accepts_clean_temp_staging(self):
        openssl = MANIFEST['find_openssl']()
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            repo = root / 'repo'
            repo.mkdir()
            # Copy minimal files needed to run sign-release in an isolated clean git repo
            (repo / 'docs/release').mkdir(parents=True)
            (repo / 'tools').mkdir()
            (repo / 'Cargo.toml').write_text((ROOT / 'Cargo.toml').read_text())
            (repo / 'docs/release/release-inventory.json').write_text((ROOT / 'docs/release/release-inventory.json').read_text())
            for f in ('sign-release.py', 'make-release-manifest.py', 'verify-release-evidence.py'):
                (repo / 'tools' / f).write_text((ROOT / 'tools' / f).read_text())
            subprocess.run(['git', 'init'], cwd=repo, check=True, capture_output=True)
            subprocess.run(['git', 'config', 'user.email', 'test@example.com'], cwd=repo, check=True, capture_output=True)
            subprocess.run(['git', 'config', 'user.name', 'Test'], cwd=repo, check=True, capture_output=True)
            subprocess.run(['git', 'add', '.'], cwd=repo, check=True, capture_output=True)
            subprocess.run(['git', 'commit', '-m', 'init'], cwd=repo, check=True, capture_output=True)
            commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()

            key = root / 'pub.pem'
            subprocess.run([openssl, 'genpkey', '-algorithm', 'ED25519', '-out', str(key)], check=True, capture_output=True)
            public = subprocess.check_output([openssl, 'pkey', '-in', str(key), '-pubout', '-outform', 'DER'])[-32:].hex()

            # Stage artifacts in external temp dir (outside git checkout)
            stage = root / 'runner-temp-stage'
            (stage / 'evidence').mkdir(parents=True)
            inv = json.loads((repo / 'docs/release/release-inventory.json').read_text())
            version = inv['version']
            installer_hashes = {}
            for t in inv['targets']:
                name = t['asset_template'].format(version=version)
                p = stage / name
                p.write_bytes(f'pkg-{name}'.encode())
                digest = hashlib.sha256(p.read_bytes()).hexdigest()
                installer_hashes[name] = digest
                (stage / f'{name}.build.json').write_text(json.dumps(dict(
                    product='DT', commit=commit, version=version,
                    os=t['os'], architecture=t['architecture'], sha256=digest,
                )))
            (stage / 'evidence/evidence.json').write_text(json.dumps(dict(
                commit=commit,
                candidate_hashes=dict(installers=installer_hashes),
            )))

            env = os.environ.copy()
            env['GM_RELEASE_PUBLIC_KEY_HEX'] = public
            env['GM_RELEASE_SIGNING_KEY_PEM'] = key.read_text()

            # Clean checkout succeeds
            res = subprocess.run([sys.executable, 'tools/sign-release.py', '--directory', str(stage)], cwd=repo, env=env, capture_output=True, text=True)
            self.assertEqual(res.returncode, 0, msg=res.stderr)
            self.assertTrue((stage / 'dt-release.json').is_file())
            self.assertTrue((stage / 'dt-release-v2.json').is_file())

            # Deliberately dirty checkout fails closed
            (repo / 'untracked.txt').write_text('dirty')
            dirty = subprocess.run([sys.executable, 'tools/sign-release.py', '--directory', str(stage)], cwd=repo, env=env, capture_output=True, text=True)
            self.assertNotEqual(dirty.returncode, 0)

    def test_coverage_inventory_test_ids_exist_in_codebase(self):
        inventory = json.loads((ROOT / 'docs/release/coverage-inventory.json').read_text())
        search_dirs = [ROOT / 'crates', ROOT / 'xtask', ROOT / 'tools', ROOT / 'clients']
        corpus = []
        for base in search_dirs:
            for path in base.rglob('*'):
                if 'node_modules' in path.parts or 'dist' in path.parts or '.expo' in path.parts:
                    continue
                if path.is_file() and path.suffix in ('.rs', '.py', '.mjs', '.cjs', '.js', '.ts', '.tsx', '.sh'):
                    corpus.append(path.read_text(errors='ignore'))
        full_text = '\n'.join(corpus)
        missing = []
        for row in inventory.get('rows', []):
            if not row.get('required'):
                continue
            for test_id in row.get('test_ids', []):
                if test_id not in full_text:
                    missing.append((row['id'], test_id))
        self.assertEqual(missing, [], f'Missing required coverage-inventory test_ids in codebase: {missing}')

    def test_docker_and_installer_contracts(self):
        dockerfile = (ROOT / 'deploy/docker/Dockerfile').read_text()
        for expected in ('--bin dt', '--bin gm', '--bin gm-daemon', '--bin dt-doctor', 'libpipewire-0.3-dev', 'USER dt', '127.0.0.1:7433'):
            self.assertIn(expected, dockerfile)
        self.assertIn('DT_WEB_DIST', dockerfile)
        build_deb = (ROOT / 'tools/installers/linux/build-deb.sh').read_text()
        self.assertIn('usr/share/dt/web', build_deb)
        nsi = (ROOT / 'tools/installers/windows/dt-installer.nsi').read_text()
        self.assertIn(r'share\dt\web', nsi)
        xtask_src = (ROOT / 'xtask/src/main.rs').read_text()
        self.assertIn('app_res_dir.join("web")', xtask_src)
        pkg_rel = (ROOT / 'tools/package-release.py').read_text()
        self.assertIn("'Resources' / 'web'", pkg_rel)
        compose = (ROOT / 'deploy/docker/docker-compose.yml').read_text()
        for expected in ('dt-server:', 'tls-proxy:', '"443:443"'):
            self.assertIn(expected, compose)
        caddy = (ROOT / 'deploy/docker/Caddyfile').read_text()
        self.assertIn('reverse_proxy', caddy)

    def test_versions_are_in_sync(self):
        import tomllib
        cargo = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
        web = json.loads((ROOT / 'clients/web/package.json').read_text())['version']
        mobile = json.loads((ROOT / 'clients/mobile/package.json').read_text())['version']
        app = json.loads((ROOT / 'clients/mobile/app.json').read_text())['expo']['version']
        rel_inv = json.loads((ROOT / 'docs/release/release-inventory.json').read_text())['version']
        cov_inv = json.loads((ROOT / 'docs/release/coverage-inventory.json').read_text())['version']
        ledger = json.loads((ROOT / 'docs/release/inference-budget-ledger.json').read_text())['release_version']
        self.assertEqual(
            {cargo, web, mobile, app, rel_inv, cov_inv, ledger},
            {cargo},
            f'version drift: cargo={cargo} web={web} mobile={mobile} app={app} rel={rel_inv} cov={cov_inv} ledger={ledger}',
        )
        self.assertIn(f'# DT v{cargo}', (ROOT / 'README.md').read_text())
        self.assertIn(f'DT v{cargo}', (ROOT / 'docs/SUPPORT_MATRIX.md').read_text())
        self.assertIn(f"const RELEASE_TAG = 'v{cargo}';", (ROOT / 'docs/index.html').read_text())
        self.assertTrue((ROOT / f'docs/releases/v{cargo}.md').is_file(), f'Missing docs/releases/v{cargo}.md')
        self.assertNotIn('1.0.0', (ROOT / '.github/workflows/ci.yml').read_text(), 'ci.yml must not hard-code a version')

    def test_signed_metadata_not_expiring_soon(self):
        from datetime import datetime, timedelta, timezone
        limit = datetime.now(timezone.utc) + timedelta(days=30)
        for rel in ('docs/release/component-catalog.json', 'docs/release/release-inventory.json'):
            p = ROOT / rel
            if not p.exists():
                continue
            exp = json.loads(p.read_text()).get('expires_at')
            if exp:
                when = datetime.fromisoformat(exp.replace('Z', '+00:00'))
                self.assertGreater(when, limit, f'{rel} expires {exp} (<30 days). Run tools/renew-update-metadata.py')


if __name__ == '__main__':
    unittest.main()

