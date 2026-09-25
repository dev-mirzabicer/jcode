#!/usr/bin/env python3
"""Real CLI Start/Stop receipt and namespace journey; no model or user work.
Run with an immutable candidate and an owned evidence parent. Never targets the
user socket or signals a PID. Cleanup uses reviewed controls in this fixture.
"""
import argparse
import concurrent.futures
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import shutil
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--output-root', required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.output_root.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix='runtime-cli-', dir=args.output_root)).resolve()
    (root / 'owned-fixture.json').write_text(json.dumps({'fixture': 'WP08 runtime CLI', 'binary': str(binary)}))
    home = root / 'home'
    home.mkdir(mode=0o700)
    evidence = []
    errors = []
    cleanup_errors = []
    # Small IPC files need a short path on macOS. Durable evidence stays above.
    ipc = tempfile.mkdtemp(prefix='jcrt-', dir='/tmp')
    (root / 'runtime').mkdir(mode=0o700)
    (Path(ipc) / 'state').symlink_to(root / 'runtime', target_is_directory=True)
    socket = Path(ipc) / 'nested' / 'runtime.sock'
    env = {key: os.environ[key] for key in ('PATH','LANG','LC_ALL','TMPDIR','TERM') if key in os.environ}
    env.update(HOME=str(home), JCODE_HOME=str(home / '.jcode'), JCODE_RUNTIME_DIR=str(Path(ipc) / 'state'), JCODE_SOCKET=str(socket), XDG_CONFIG_HOME=str(home / '.config'), XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'), JCODE_DEFERRED_AUTH_BOOTSTRAP='1', JCODE_NO_UPDATE='1')
    prefix = [str(binary), '--no-update', '--socket', str(socket)]

    def invoke(*command, success=True):
        run = subprocess.run([*prefix, *command], env=env, cwd=root, capture_output=True, text=True, timeout=120)
        entry = {'command': command, 'exit': run.returncode, 'stdout': run.stdout, 'stderr': run.stderr}
        evidence.append(entry)
        if success and run.returncode:
            raise AssertionError(entry)
        if not success:
            assert run.returncode != 0, entry
            return entry
        return json.loads(run.stdout)

    def stop():
        report = invoke('runtime','status','--json')
        if not report['live_response']:
            assert report['coordinator_owned'] is not True, report
            return
        review = invoke('runtime','stop','--strategy','interrupt','--tasks','stop','--json')
        review_id = review['response']['value']['id']
        begun = invoke('runtime','confirm',review_id,'--request',str(uuid.uuid4()),'--json')
        operation = begun['response']['value']['id']
        invoke('runtime','wait',operation,'--timeout-seconds','30','--json')

    try:
        absent = invoke('runtime','status','--json')
        assert not absent['live_response'] and not socket.parent.exists(), absent
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            starts = list(pool.map(lambda _: invoke('runtime','start','--json'), range(2)))
        identities = {entry['response']['value']['runtime'] for entry in starts}
        assert len(identities) == 1 and None not in identities, starts
        original = identities.pop()
        invoke('server','stop','--force','--json',success=False)
        assert invoke('runtime','status','--json')['response']['value']['runtime'] == original
        review = invoke('runtime','stop','--strategy','finish-current','--json')
        assert not invoke('runtime','status','--json')['response']['value']['desired_stopped']
        request = review['confirm_request']
        review_id = review['response']['value']['id']
        begun = invoke('runtime','confirm',review_id,'--request',request,'--json')
        operation = begun['response']['value']['id']
        completed = invoke('runtime','wait',operation,'--timeout-seconds','30','--json')
        assert completed['response']['value']['phase'] == 'stopped', completed
        assert completed['coordinator_owned'] is False
        before = invoke('runtime','status','--json')
        assert not before['live_response'] and before['response']['value']['desired_stopped']
        replay = invoke('runtime','confirm',review_id,'--request',request,'--json')
        assert replay['response']['value']['id'] == operation and not replay['live_response']
        assert not socket.exists(), 'Offline reads restarted the runtime'
        # Direct serve cannot bypass desired-stop, independently of spawner policy.
        invoke('serve', success=False)
        assert not socket.exists()
        restarted = invoke('runtime','start','--json')
        assert restarted['response']['value']['runtime'] != original
        assert not restarted['response']['value']['desired_stopped']
        assert invoke('runtime','inspect',operation,'--json')['response']['value']['phase'] == 'stopped'
        assert not list((home / '.jcode' / 'sessions').glob('*.json')), 'Administrative controls created a Session'
    except Exception as error:
        errors.append(repr(error))
    finally:
        try:
            stop()
        except Exception as error:
            cleanup_errors.append(repr(error))
        try:
            # Check exact owned coordinator/worker leases, not PID absence.
            for endpoint_file in (home / '.jcode' / 'execution' / 'runtimes').glob('*.json'):
                endpoint = json.loads(endpoint_file.read_text())
                if 'lease_path' not in endpoint:
                    continue
                lease_path = Path(endpoint['lease_path'])
                assert lease_path.is_relative_to(home), 'Fixture lease escaped its state root'
                fd = os.open(lease_path, os.O_RDWR | os.O_NOFOLLOW)
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    os.close(fd)
        except Exception as error:
            cleanup_errors.append(repr(error))
        if not cleanup_errors:
            try:
                shutil.rmtree(ipc)
            except Exception as error:
                cleanup_errors.append(repr(error))
        with binary.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        result = {'ipc': ipc, 'binary': str(binary), 'sha256': digest, 'root': str(root), 'errors': errors, 'cleanup_errors': cleanup_errors, 'commands': evidence}
        (root / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps({'root': str(root), 'passed': not errors and not cleanup_errors, 'errors': errors, 'cleanup_errors': cleanup_errors}))
    if errors or cleanup_errors:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
