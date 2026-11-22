#!/usr/bin/env python3
"""Validate publisher-signed native acceptance receipts. Fixtures cannot certify a release."""
import argparse
import datetime
import hashlib
import json
import math
import pathlib
import re
import runpy
import subprocess
import tempfile
import tomllib

HERE = pathlib.Path(__file__).resolve().parent
MANIFEST = runpy.run_path(str(HERE / 'make-release-manifest.py'))
load_inventory = MANIFEST['load_inventory']
find_openssl = MANIFEST['find_openssl']


def expected_targets_from_inventory():
    inv = load_inventory()
    targets = set()
    for t in inv['targets']:
        for session in t['sessions']:
            targets.add((t['os'], t['architecture'], session))
    return targets


TARGETS = expected_targets_from_inventory()
CHECKS = {
    'onboarding', 'permission_revocation', 'multi_monitor', 'approvals_handoff', 'restart_persistence',
    'accessibility', 'ime', 'clean_install', 'upgrade', 'manual_recovery', 'service_lifecycle', 'uninstall',
    'vm_offline_boot', 'vm_cli_boot', 'cli_claude', 'cli_codex', 'cli_grok', 'cli_cancellation',
    'cli_output_limit', 'cli_egress_denied', 'cli_credentials_isolated', 'real_vlm_qwen', 'real_vlm_smol',
    'api_meta', 'api_xai', 'citation_verification', 'patch_review_apply',
}

RAW_METRIC_RULES = {
    'idle_tree_mib': ('max', 50.0, 'idle_tree_mib'),
    'ui_tree_mib': ('max', 120.0, 'ui_tree_mib'),
    'capture_ms': ('p99', 2.0, 'capture_p99_ms'),
    'ax_ms': ('p95', 30.0, 'ax_p95_ms'),
    'jpeg_ms': ('p95', 15.0, 'jpeg_p95_ms'),
    'dispatch_ms': ('p95', 5.0, 'dispatch_p95_ms'),
    'kill_ms': ('max', 100.0, 'kill_ms'),
    'step_ms': ('p95', 4000.0, 'step_p95_ms'),
}


def percentile(values: list[float], pct: float) -> float:
    ordered = sorted(values)
    idx = max(0, min(len(ordered) - 1, math.ceil(len(ordered) * pct) - 1))
    return ordered[idx]


def verify(raw, signature_hex, public_hex):
    key = bytes.fromhex(public_hex)
    signature = bytes.fromhex(signature_hex.strip())
    if len(key) != 32 or len(signature) != 64:
        raise ValueError('Invalid Ed25519 key/signature')
    openssl = find_openssl()
    with tempfile.TemporaryDirectory(prefix='dt-evidence-') as folder:
        root = pathlib.Path(folder)
        (root / 'key.der').write_bytes(bytes.fromhex('302a300506032b6570032100') + key)
        (root / 'receipt').write_bytes(raw)
        (root / 'signature').write_bytes(signature)
        subprocess.run([openssl, 'pkeyutl', '-verify', '-pubin', '-keyform', 'DER', '-inkey', str(root / 'key.der'),
                        '-rawin', '-in', str(root / 'receipt'), '-sigfile', str(root / 'signature')], check=True, capture_output=True)


def validate(data, commit, root):
    if data.get('schema') != 1 or data.get('commit') != commit or data.get('source') != 'native':
        raise ValueError('Evidence must describe native execution of this exact commit')
    recorded = datetime.datetime.fromisoformat(data['recorded_at'].replace('Z', '+00:00'))
    if recorded.tzinfo is None:
        raise ValueError('Evidence timestamp requires a timezone')
    age = datetime.datetime.now(datetime.timezone.utc) - recorded
    if not datetime.timedelta(0) <= age <= datetime.timedelta(days=30):
        raise ValueError('Native evidence must be current (within 30 days)')

    docs_dir = pathlib.Path(__file__).resolve().parent.parent / 'docs/release'
    catalog = json.loads((docs_dir / 'acceptance-tasks.json').read_text())
    required_tasks = {task['id']: task['workflow'] for task in catalog}

    inv_path = docs_dir / 'coverage-inventory.json'
    if inv_path.is_file():
        inventory = json.loads(inv_path.read_text())
        required_rows = {row['id'] for row in inventory.get('rows', []) if row.get('required')}
        attested_rows = data.get('coverage_rows', {})
        if not isinstance(attested_rows, dict) or not required_rows.issubset(attested_rows.keys()):
            raise ValueError('Evidence is missing one or more required coverage-inventory rows')
        if any(attested_rows[r] is not True for r in required_rows):
            raise ValueError('One or more required coverage-inventory rows did not pass')

    release_inv = load_inventory()
    version = tomllib.loads((docs_dir.parent.parent / 'Cargo.toml').read_text())['workspace']['package']['version']
    candidate = data.get('candidate_hashes')
    if not isinstance(candidate, dict) or set(candidate) != {'installers', 'container', 'components', 'mobile'}:
        raise ValueError('Evidence requires candidate installer, container, component, and mobile hashes')
    installer_names = {t['asset_template'].format(version=version) for t in release_inv['targets']}
    component_names = set(release_inv['components'])
    for key, expected in [('installers', installer_names), ('components', component_names), ('mobile', {'ios', 'android'})]:
        hashes = candidate[key]
        if not isinstance(hashes, dict) or set(hashes) != expected:
            raise ValueError(f'Evidence requires exact candidate {key}')
        if not all(isinstance(value, str) and re.fullmatch(r'[0-9a-f]{64}', value) for value in hashes.values()):
            raise ValueError(f'Invalid SHA-256 digest in candidate {key}')
    if not isinstance(candidate['container'], str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', candidate['container']):
        raise ValueError('Invalid candidate container digest')

    expected_targets = expected_targets_from_inventory()
    targets = set()
    for report in data.get('targets', []):
        target = tuple(report.get(key) for key in ('os', 'architecture', 'session'))
        if target not in expected_targets or target in targets:
            raise ValueError('Unknown or duplicate certification target')
        targets.add(target)
        if set(report.get('checks', {})) != CHECKS or any(v is not True for v in report['checks'].values()):
            raise ValueError(f'Missing or failed native checks for {target}')
        soak = report.get('soak')
        if not isinstance(soak, dict):
            raise ValueError(f'Missing proactive soak for {target}')
        started = datetime.datetime.fromisoformat(soak['started_at'].replace('Z', '+00:00'))
        ended = datetime.datetime.fromisoformat(soak['ended_at'].replace('Z', '+00:00'))
        if started.tzinfo is None or ended.tzinfo is None or ended > recorded or ended - started < datetime.timedelta(days=7):
            raise ValueError(f'Proactive soak must span at least seven days before evidence was recorded: {target}')
        ratings, useful, daily_peak = (soak.get(key) for key in ('ratings', 'useful', 'max_notices_per_day'))
        if any(type(value) is not int for value in (ratings, useful, daily_peak)) or ratings < 20 or not 0 <= daily_peak <= 3 or not 0 <= useful <= ratings or useful * 5 < ratings * 4:
            raise ValueError(f'Proactive usefulness or notice-rate gate failed for {target}')
        if any(soak.get(key) is not True for key in ('quiet_hours', 'opt_out', 'budget')):
            raise ValueError(f'Proactive quiet hours, opt-out, or budget gate failed for {target}')
        runs = report.get('runs', [])
        if len(runs) != 3:
            raise ValueError('Three consecutive native runs are required')
        for run in runs:
            tasks = run.get('tasks', [])
            if len(tasks) != 30 or len({task['id'] for task in tasks}) != 30:
                raise ValueError('Each run requires 30 unique tasks')
            if {task['id']: task.get('workflow') for task in tasks} != required_tasks:
                raise ValueError('Tasks must match the reviewed acceptance catalog')
            counts = {workflow: sum(t.get('workflow') == workflow for t in tasks) for workflow in ('research', 'coding', 'desktop')}
            if set(counts.values()) != {10} or any(type(t.get('success')) is not bool or type(t.get('unapproved_irreversible')) is not bool for t in tasks):
                raise ValueError('Need ten measured tasks per workflow with explicit outcomes')
            if sum(t['success'] for t in tasks) < 30 or any(t['unapproved_irreversible'] for t in tasks):
                raise ValueError('Quality/safety gate failed (requires 30/30 passes and 0 unapproved irreversible actions)')

        measured = report.get('measurements', {})
        if measured.get('installed_binary_hashes_verified') is not True:
            raise ValueError(f'Installed hash-verified binary check missing/failed for {target}')
        if measured.get('jpeg_decoded_verified') is not True:
            raise ValueError(f'Decoded JPEG dimension verification missing/failed for {target}')
        raw_samples = measured.get('raw_samples')
        if not isinstance(raw_samples, dict):
            raise ValueError(f'Per-metric raw_samples missing for {target}')
        for metric_key, (agg_kind, limit, summary_key) in RAW_METRIC_RULES.items():
            samples = raw_samples.get(metric_key)
            if not isinstance(samples, list) or len(samples) < 30:
                raise ValueError(f'At least 30 raw samples required for {target} metric {metric_key}')
            if any(type(v) not in (int, float) or not math.isfinite(v) or v <= 0 for v in samples):
                raise ValueError(f'Invalid raw sample value in {target} metric {metric_key}')
            computed = max(samples) if agg_kind == 'max' else percentile(samples, 0.99 if agg_kind == 'p99' else 0.95)
            if not 0 < computed < limit:
                raise ValueError(f'Native performance gate failed from raw samples: {target} {metric_key}={computed}')
            reported_val = measured.get(summary_key)
            if type(reported_val) not in (int, float) or abs(reported_val - computed) > 1e-3:
                raise ValueError(f'Reported summary {summary_key} disagrees with raw samples for {target}')
        step_p50_computed = percentile(raw_samples['step_ms'], 0.50)
        if not 0 < step_p50_computed < 1500.0:
            raise ValueError(f'Native step p50 gate failed for {target}')
        if type(measured.get('step_p50_ms')) not in (int, float) or abs(measured['step_p50_ms'] - step_p50_computed) > 1e-3:
            raise ValueError(f'Reported step_p50_ms disagrees with raw samples for {target}')

        artifacts = report.get('artifacts', [])
        if not artifacts:
            raise ValueError('Evidence requires referenced logs/results')
        for artifact in artifacts:
            relative = pathlib.PurePosixPath(artifact['path'])
            if relative.is_absolute() or '..' in relative.parts or '\\' in str(relative):
                raise ValueError('Unsafe evidence artifact path')
            path = root.joinpath(*relative.parts)
            if path.is_symlink() or root.resolve() not in path.resolve().parents or not path.is_file():
                raise ValueError('Evidence artifact must be a contained regular file')
            if path.stat().st_size > 128 * 1024**2:
                raise ValueError('Evidence artifact exceeds size limit')
            with path.open('rb') as stream:
                hasher = hashlib.sha256()
                for chunk in iter(lambda: stream.read(65536), b''):
                    hasher.update(chunk)
                digest = hasher.hexdigest()
            if digest != artifact['sha256']:
                raise ValueError('Evidence artifact checksum mismatch')
    if targets != expected_targets:
        raise ValueError('Native evidence is missing one or more release targets')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=pathlib.Path, required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--public-key-hex', required=True)
    args = parser.parse_args()
    receipt = args.directory / 'evidence.json'
    if receipt.stat().st_size > 2 * 1024**2:
        parser.error('Evidence receipt exceeds 2 MiB')
    raw = receipt.read_bytes()
    verify(raw, (args.directory / 'evidence.sig').read_text(), args.public_key_hex)
    validate(json.loads(raw), args.commit, args.directory)
    print('Native release evidence verified for all targets. No fixture result was used.')


if __name__ == '__main__':
    main()
