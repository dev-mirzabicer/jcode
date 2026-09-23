#!/usr/bin/env python3
"""WP-06 authenticated Git/LFS acceptance. Private HOME, loopback only, no paid model.

Run through scripts/run_isolated_test.py. The credential helper returns synthetic
fixture bytes, and neither this fixture nor Jcode opens the real user's keychain.
"""
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import uuid

import test_instruction_manager as f

if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')

TOKEN = 'synthetic-fixture-password-only'
USER = 'fixture'
AUTH = 'Basic ' + base64.b64encode(f'{USER}:{TOKEN}'.encode()).decode()
objects = {}
requests = []
helper_calls = f.ROOT / 'credential-helper-calls'


class AuthServer(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(f.ROOT), **kwargs)

    def log_message(self, *args):
        pass

    def authenticated(self):
        allowed = self.headers.get('Authorization') == AUTH
        requests.append({'path': self.path, 'authorized': allowed,
                         'method': self.command})
        if allowed:
            return True
        self.send_response(401)
        self.send_header('WWW-Authenticate', 'Basic realm="owned-fixture"')
        self.send_header('Content-Length', '0')
        self.end_headers()
        return False

    def do_POST(self):
        if not self.authenticated():
            return
        if self.path != '/lfs/objects/batch':
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert request['operation'] == 'download'
        result = []
        for item in request['objects']:
            oid = item['oid']
            payload = objects.get(oid)
            result.append({'oid': oid, 'size': len(payload), 'actions': {
                'download': {'href': f'http://127.0.0.1:{auth.server_port}/lfs/objects/{oid}'}}}
                if payload is not None else {'oid': oid, 'size': item['size'],
                                             'error': {'code': 404, 'message': 'missing fixture object'}})
        body = json.dumps({'transfer': 'basic', 'objects': result}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/vnd.git-lfs+json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if not self.authenticated():
            return
        if self.path.startswith('/lfs/objects/'):
            oid = self.path.rsplit('/', 1)[-1]
            payload = objects.get(oid)
            if payload is None:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Type', 'application/octet-stream')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        super().do_GET()


auth = http.server.ThreadingHTTPServer(('127.0.0.1', 0), AuthServer)
threading.Thread(target=auth.serve_forever, daemon=True).start()
f.env['GIT_CONFIG_NOSYSTEM'] = '1'
f.env['GIT_TERMINAL_PROMPT'] = '0'
fixture_home = Path(os.environ['HOME'])
config = fixture_home / '.gitconfig'
helper = f.ROOT / 'owned-git-credential-helper'
helper.write_text(f'''#!/bin/sh
printf '%s\\n' "$1" >> '{helper_calls}'
if [ "$1" = get ]; then
    printf 'username={USER}\\npassword={TOKEN}\\n\\n'
fi
''')
helper.chmod(0o700)
safety_sentinel = f.ROOT / 'unrequested-global-executable-ran'
danger = f.ROOT / 'unrequested-global-executable'
danger.write_text(f'#!/bin/sh\ntouch "{safety_sentinel}"\nexit 98\n')
danger.chmod(0o700)
config_with_helper = (f'[credential "http://127.0.0.1:{auth.server_port}"]\n'
                      f'\thelper = {helper}\n'
                      f'[core]\n\thooksPath = {danger}\n'
                      f'[filter "lfs"]\n\tprocess = {danger}\n\tsmudge = {danger}\n')


def git(root, *args):
    result = subprocess.run(['git', '-C', str(root), *args], env=f.env,
                            capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (args, result.returncode,
                                    result.stdout[-300:], result.stderr[-300:])
    return result.stdout.strip()


def uid():
    return str(uuid.uuid4())


def rpc(action, expected=None, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type': 'workspace', 'id': rid, 'request': {'action': action, **fields}})
    event = f.until(lambda e: e.get('id') == rid and e.get('type') in ('workspace_response', 'error'))
    assert event['type'] == 'workspace_response', event
    response = event['response']
    if expected:
        assert response['kind'] == expected, response
    return response['value'] if expected else response


def status():
    return rpc('status', 'status')


def change(action, **fields):
    review = rpc('review', 'review', expected_revision=status()['revision'],
                 change={'action': action, **fields})
    receipt = rpc('apply', 'receipt', request=uid(), review=review['id'])
    assert not receipt['issues'], receipt
    return receipt['targets'][0]['id']


def await_clone(rid):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        record = rpc('inspect_clone', 'clone', request=rid)
        if record['state'] in ('ready', 'preparation_failed', 'cancelled', 'recovery_required'):
            assert record['state'] == 'ready', record
            break
        time.sleep(.15)
    else:
        raise AssertionError(('clone did not complete', rid))
    assert len(record['output_runs']) == 1
    run_id = record['output_runs'][0]
    for _ in range(300):
        out = rpc('clone_output', 'clone_output', clone=rid,
                  request={'action': 'inspect', 'run_id': run_id})
        assert out['kind'] == 'status', out
        run = out['run']
        if run['state'] in ('completed', 'failed', 'cancelled', 'interrupted'):
            assert run['state'] == 'completed' and run['complete'] and run['output_bytes'] > 0, run
            return record, run_id
        time.sleep(.1)
    raise AssertionError(('retained clone output not terminal', run_id))


result = {}
try:
    source = f.project
    git(source, 'init', '-q', '-b', 'main')
    git(source, 'lfs', 'install', '--local', '--skip-repo')
    git(source, 'lfs', 'track', '*.bin')
    payload = b'complete fixture LFS bytes, not a Git pointer\n'
    (source / 'payload.bin').write_bytes(payload)
    (source / '.lfsconfig').write_text(f'[lfs]\n\turl = http://127.0.0.1:{auth.server_port}/lfs\n')
    git(source, 'add', '.')
    git(source, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'main with LFS')
    main = git(source, 'rev-parse', 'HEAD')
    git(source, 'switch', '-q', '-c', 'topic')
    (source / 'topic-only.txt').write_text('unique local source history\n')
    git(source, 'add', 'topic-only.txt')
    git(source, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'topic')
    topic = git(source, 'rev-parse', 'HEAD')
    git(source, 'switch', '-q', 'main')
    for item in (source / '.git/lfs/objects').rglob('*'):
        if item.is_file() and len(item.name) == 64:
            data = item.read_bytes()
            assert hashlib.sha256(data).hexdigest() == item.name
            objects[item.name] = data
    assert objects and payload in objects.values()
    bare = f.ROOT / 'remote.git'
    subprocess.run(['git', 'clone', '--bare', '-q', str(source), str(bare)],
                   env=f.env, capture_output=True, check=True, timeout=30)
    git(bare, 'update-server-info')
    url = f'http://127.0.0.1:{auth.server_port}/remote.git'
    lfs_url = f'http://127.0.0.1:{auth.server_port}/lfs'
    config.write_text(config_with_helper)
    ordinary = subprocess.run(['git', 'ls-remote', url], env=f.env,
                              capture_output=True, timeout=30)
    assert ordinary.returncode == 0 and main.encode() in ordinary.stdout, ordinary.stderr[-300:]
    before_review = helper_calls.read_text().splitlines()
    assert 'get' in before_review

    f.start()
    rpc('initialize', 'status', request=uid())
    volume = [v for v in rpc('volumes', 'volumes') if v['writable']
              and Path(v['mount']).stat().st_dev == f.ROOT.stat().st_dev][0]
    project = change('create_project', name='credential-test')
    repository = change('create_repository', name='repo', remotes=[])
    change('associate_repository', project=project, repository=repository)
    destination = f.ROOT / 'private-checkout'
    spec = {'home': {'kind': 'project', 'id': project}, 'repository': repository,
            'name': 'private-checkout', 'source': {'kind': 'remote', 'url': url},
            'base': {'kind': 'branch', 'name': 'main'}, 'branch': {'kind': 'keep_name'},
            'remotes': [], 'destination': {'kind': 'custom',
                                         'volume_uuid': volume['uuid'], 'path': str(destination)},
            'submodules': False, 'lfs': True, 'trusted_lfs_urls': [lfs_url]}
    review = rpc('review_clone', 'clone_review', expected_revision=status()['revision'], spec=spec)
    assert review['source_commit'] == main
    assert len(helper_calls.read_text().splitlines()) > len(before_review), 'selected helper not used at review'
    assert TOKEN not in json.dumps(review)
    rid = uid()
    helper_before_unavailable = helper_calls.read_text().splitlines()
    config.write_text('[user]\n\tname = Fixture Without Credentials\n')
    unavailable = rpc('begin_clone', request=rid, review=review['id'])
    assert unavailable['kind'] == 'error' and unavailable['value']['code'] == 'recovery_required', unavailable
    assert not destination.exists()
    assert helper_calls.read_text().splitlines() == helper_before_unavailable
    assert rpc('inspect_clone', request=rid)['kind'] == 'error'
    config.write_text(config_with_helper)
    initial = rpc('begin_clone', 'clone', request=rid, review=review['id'])
    assert initial['request'] == rid
    ready, output_run = await_clone(rid)
    assert ready['state'] == 'ready' and ready['location'] == initial['location']
    assert (destination / 'payload.bin').read_bytes() == payload
    assert git(destination, 'rev-parse', 'refs/remotes/origin/topic') == topic
    assert git(destination, 'config', '--get', 'branch.main.remote') == 'origin'
    assert git(destination, 'remote', 'get-url', 'origin') == url
    assert not (destination / '.git/objects/info/alternates').exists()
    assert not safety_sentinel.exists(), 'unrequested global hook/filter ran'
    git(destination, 'fsck', '--full', '--no-reflogs')
    authenticated_git = [r for r in requests if r['authorized'] and r['path'].startswith('/remote.git')]
    authenticated_lfs = [r for r in requests if r['authorized'] and r['path'].startswith('/lfs/')]
    assert authenticated_git and any(r['method'] == 'POST' for r in authenticated_lfs)
    assert any(r['method'] == 'GET' for r in authenticated_lfs)
    assert 'get' in helper_calls.read_text().splitlines()
    # A genuinely unavailable helper must fail noninteractively before review
    # or allocation. The prompt/checkout destination is never fabricated.
    config.write_text('[user]\n\tname = Fixture Without Credentials\n')
    helper_before_final_review = helper_calls.read_text().splitlines()
    failed_spec = dict(spec, name='unavailable', destination=dict(spec['destination'],
                                                                path=str(f.ROOT / 'unavailable')))
    refused = rpc('review_clone', expected_revision=status()['revision'], spec=failed_spec)
    assert refused['kind'] == 'error' and refused['value']['code'] == 'recovery_required', refused
    assert helper_calls.read_text().splitlines() == helper_before_final_review
    assert not (f.ROOT / 'unavailable').exists()

    workspace_root = f.ROOT / 'runtime/durable-state/workspace'
    for file in workspace_root.rglob('*'):
        if file.is_file():
            assert TOKEN.encode() not in file.read_bytes(), file
    assert TOKEN not in json.dumps(f.events)
    for file in (f.home / 'execution/data').rglob('*'):
        if file.is_file():
            assert TOKEN.encode() not in file.read_bytes(), file
    assert TOKEN.encode() not in (f.ROOT / 'server.log').read_bytes()
    assert not f.posts
    result = {'binary': f.BIN, 'reviewed_commit': main, 'topic': topic,
              'helper_actions': helper_calls.read_text().splitlines(),
              'authenticated_git_requests': len(authenticated_git),
              'authenticated_lfs_requests': len(authenticated_lfs),
              'retained_run': output_run, 'unavailable_begin': unavailable['value']['code'],
              'unavailable_review': refused['value']['code'],
              'secrets_absent_from_catalog_events_output': True,
              'provider_requests': len(f.posts), 'managed_rollout': False}
    (f.ROOT / 'result.json').write_text(json.dumps(result, indent=2))
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
            f.proc.wait(timeout=15)
    auth.shutdown()
    f.http.shutdown()
    f.log.close()
    (f.ROOT / 'cleanup.json').write_text(json.dumps({
        'daemon_exit': f.proc.poll() if f.proc else None,
        'owned_daemon_terminal': f.proc is None or f.proc.poll() is not None,
        'provider_requests': len(f.posts), 'fixture_root': str(f.ROOT)
    }, indent=2))
    print('artifacts=' + str(f.ROOT), flush=True)
