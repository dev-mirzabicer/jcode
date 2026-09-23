#!/usr/bin/env python3
"""Cancel an actually running owned clone in an isolated daemon and disposable Git fixture.

Run through run_isolated_test.py with --binary and --artifact-dir. The shell
wrapper delays only the reviewed fixture clone, never real Git/user work.
"""
import json
import os
import shutil
import subprocess
import time
import uuid
from pathlib import Path

import test_instruction_manager as f

if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Use scripts/run_isolated_test.py')


def uid():
    return str(uuid.uuid4())


def rpc(action, expected, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type': 'workspace', 'id': rid, 'request': {'action': action, **fields}})
    event = f.until(lambda e: e.get('id') == rid and e.get('type') in ('workspace_response', 'error'))
    assert event['type'] == 'workspace_response', event
    response = event['response']
    assert response['kind'] == expected, response
    return response['value']


def event(kind, expected, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type': kind, 'id': rid, **fields})
    reply = f.until(lambda e: e.get('id') == rid and e.get('type') in (expected, 'error'))
    assert reply['type'] == expected, reply
    return reply


def status():
    return rpc('status', 'status')


def change(action, **fields):
    review = rpc('review', 'review', expected_revision=status()['revision'],
                 change={'action': action, **fields})
    return rpc('apply', 'receipt', request=uid(), review=review['id'])['targets']


def git(root, *args):
    result = subprocess.run([shutil.which('git'), '-C', str(root), *args],
                            capture_output=True, env=f.env, text=True, timeout=20)
    assert result.returncode == 0, (args, result.stdout, result.stderr)
    return result.stdout.strip()


result = {}
try:
    source = f.project
    git(source, 'init', '-q', '-b', 'main')
    (source / 'data').write_text('committed source')
    git(source, 'add', 'data')
    git(source, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'source')
    real_git = shutil.which('git')
    assert real_git and Path(real_git).is_file()
    marker = f.ROOT / 'owned-git-spawned'
    bin_dir = f.ROOT / 'bin'
    bin_dir.mkdir()
    wrapper = bin_dir / 'git'
    wrapper.write_text('#!/bin/sh\n'
        'clone=0; isolated=0; stage=0\n'
        'for arg in "$@"; do\n'
        '  [ "$arg" = "clone" ] && clone=1\n'
        '  [ "$arg" = "--no-hardlinks" ] && isolated=1\n'
        '  case "$arg" in */.jcode-clone-*) stage=1;; esac\n'
        'done\n'
        'if [ "$clone" = 1 ] && [ "$isolated" = 1 ] && [ "$stage" = 1 ]; then\n'
        '  printf "%s\\n" "$$" > "$WP06_CLONE_MARKER"\n'
        '  printf "fixture clone entered owned process group\\n" >&2\n'
        '  exec /bin/sleep 30\n'
        'fi\n'
        f'exec "{real_git}" "$@"\n')
    wrapper.chmod(0o700)
    f.env.update(PATH=str(bin_dir) + os.pathsep + f.env.get('PATH', ''),
                 WP06_CLONE_MARKER=str(marker))
    f.start()
    probe = event('workspace_probe', 'workspace_capabilities')
    assert probe['checkout_version'] == 1 and probe['managed_rollout'] is False
    rpc('initialize', 'status', request=uid())
    volumes = rpc('volumes', 'volumes')
    volume, = [v for v in volumes if v['writable'] and
               os.stat(v['mount']).st_dev == os.stat(f.ROOT).st_dev]
    project, = change('create_project', name='Cancel fixture')
    repository, = change('create_repository', name='Repository', remotes=[])
    change('associate_repository', project=project['id'], repository=repository['id'])
    destination = f.ROOT / 'cancelled-checkout'
    spec = dict(home={'kind': 'project', 'id': project['id']},
                repository=repository['id'], name='cancelled checkout',
                source={'kind': 'local', 'path': str(source)},
                base={'kind': 'branch', 'name': 'main'},
                branch={'kind': 'detached'}, remotes=[],
                destination={'kind': 'custom', 'volume_uuid': volume['uuid'],
                             'path': str(destination)},
                submodules=False, lfs=False)
    review = rpc('review_clone', 'clone_review', expected_revision=status()['revision'], spec=spec)
    rid = uid()
    initial = rpc('begin_clone', 'clone', request=rid, review=review['id'])
    assert initial['state'] in ('pending', 'acquiring') and not destination.exists(), initial
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline and not marker.exists():
        assert not destination.exists(), 'fixture clone published before cancellation'
        time.sleep(.05)
    assert marker.exists(), 'fixture missed actual Git process launch'
    owned_pid = int(marker.read_text().strip())
    waiting = rpc('inspect_clone', 'clone', request=rid)
    assert waiting['state'] == 'acquiring' and waiting['output_runs'], waiting
    pending = rpc('cancel_clone', 'clone', request=rid)
    assert pending['cancel_requested'], pending
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        final = rpc('inspect_clone', 'clone', request=rid)
        if final['state'] == 'cancelled' and final['stage'] is None:
            break
        time.sleep(.1)
    else:
        raise AssertionError(('cancellation never reached a truthful terminal/cleanup state', final))
    assert not destination.exists()
    assert not list(f.ROOT.glob('.jcode-clone-*'))
    run_id, = final['output_runs']
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        record = rpc('clone_output', 'clone_output', clone=rid,
                     request={'action': 'inspect', 'run_id': run_id})
        assert record['kind'] == 'status', record
        run = record['run']
        if run['state'] in ('completed', 'failed', 'cancelled', 'interrupted'):
            break
        time.sleep(.1)
    assert run['state'] == 'failed' and run['complete'] and run['output_bytes'] > 0, run
    body = rpc('clone_output', 'clone_output', clone=rid,
               request={'action': 'read', 'run_id': run_id, 'content': 'output'})
    assert body['kind'] == 'content' and 'fixture clone entered owned process group' in json.dumps(body), body
    wrong = rpc('clone_output', 'error', clone=uid(),
                request={'action': 'inspect', 'run_id': run_id})
    assert wrong['code'] in ('invalid_identity', 'corrupt_state'), wrong
    forbidden = rpc('clone_output', 'error', clone=rid,
                    request={'action': 'stop', 'run_id': run_id})
    assert forbidden['code'] == 'unsupported_capability', forbidden
    try:
        os.kill(owned_pid, 0)
    except ProcessLookupError:
        pass
    else:
        raise AssertionError(f'owned fixture process {owned_pid} still exists after cancellation')
    replay = rpc('resume_clone', 'clone', request=rid)
    assert replay['state'] == 'cancelled' and replay['output_runs'] == [run_id], replay
    assert marker.read_text().strip() == str(owned_pid)
    assert not f.posts
    assert not list((f.home / 'sessions').glob('*.json')), 'admin operation created a provisional Session'
    result = {'binary': subprocess.check_output([f.BIN, '--version'], text=True).strip(),
              'artifact': str(f.ROOT), 'cancelled_request': rid,
              'clone_pid_stopped': owned_pid, 'retained_run': run_id,
              'terminal_output': run['state'], 'empty_owned_stage_removed': True,
              'duplicate_resume_did_not_restart_git': True,
              'provider_requests': len(f.posts), 'managed_rollout': False}
    (f.ROOT / 'clone-cancel-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:
            f.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            f.proc.kill()
            f.proc.wait()
    f.http.shutdown()
    f.log.close()
    (f.ROOT / 'events.json').write_text(json.dumps(f.events, indent=2))
    (f.ROOT / 'provider-posts.json').write_text(json.dumps(f.posts, indent=2))
    (f.ROOT / 'cleanup.json').write_text(json.dumps({
        'owned_daemon_terminal': f.proc is None or f.proc.poll() is not None,
        'fixture_git_pid': marker.read_text().strip() if marker.exists() else None,
    }, indent=2))
    print('artifacts=' + str(f.ROOT))
