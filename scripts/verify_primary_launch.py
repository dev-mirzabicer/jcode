#!/usr/bin/env python3
"""Public primary creation through isolated daemon, CLI, REPL, ACP and Harness.
No paid provider or real user state is used. Run through run_isolated_test.py.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, socket, subprocess, tempfile, time, uuid, select
from pathlib import Path
import test_instruction_manager as f
ipc = Path(tempfile.mkdtemp(prefix='pl-', dir=os.environ['JCODE_RUNTIME_DIR']))
old_socket = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if arg == old_socket else arg for arg in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
children = []
channels = []
result = {}

def uid():
    return str(uuid.uuid4())

def rpc(kind, **fields):
    f.counter += 1
    identity = f.counter
    f.send({'type': kind, 'id': identity, **fields})
    return f.until(lambda e: e.get('id') == identity and e.get('type') != 'ack')

def workspace(action, kind, **fields):
    event = rpc('workspace', request={'action': action, **fields})
    assert event['type'] == 'workspace_response', event
    response = event['response']
    assert response['kind'] == kind, response
    return response['value']

def status():
    return workspace('status', 'status')

def change(action, **fields):
    review = workspace('review', 'review', expected_revision=status()['revision'], change={'action': action, **fields})
    receipt = workspace('apply', 'receipt', request=uid(), review=review['id'])
    assert not receipt['issues'], receipt
    return receipt['targets'][0]

def spec(placement, path, empty=False):
    return {'request': uid(), 'expected_revision': status()['revision'], 'input': {'placement': placement, 'cwd': {'kind': 'create_empty' if empty else 'existing', 'path': str(path), **({'home': None} if empty else {})}, 'agent': None, 'model': None, 'selfdev': False}}

def launch(request):
    event = rpc('primary_launch', request=request)
    assert event['type'] == 'primary_launch_response', event
    response = event['response']
    assert response['status'] == 'launched', response
    record = response['record']
    assert record['state'] == 'complete' and record['request'] == request['request'], record
    return record

def connect(path):
    client = socket.socket(socket.AF_UNIX)
    client.settimeout(120)
    client.connect(str(path))
    reader = client.makefile('rb')
    channels.append((client, reader))
    return (client, reader)

def exchange(channel, value, key, expected):
    client, reader = channel
    client.sendall((json.dumps(value) + '\n').encode())
    while True:
        line = reader.readline()
        assert line, 'closed transport'
        event = json.loads(line)
        if event.get(key) == value['id'] and event.get('type') != 'ack':
            assert event.get('ev', event.get('type')) == expected, event
            return event

def complete(self):
    body = self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode()
    f.posts.append(body)
    chunks = [{'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': 'Synthetic primary caller completed'}, 'finish_reason': None}]}, {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 20, 'completion_tokens': 5, 'total_tokens': 25}}]
    payload = (''.join(('data: ' + json.dumps(chunk) + '\n\n' for chunk in chunks)) + 'data: [DONE]\n\n').encode()
    self.send_response(200)
    self.send_header('Content-Type', 'text/event-stream')
    self.send_header('Content-Length', str(len(payload)))
    self.end_headers()
    self.wfile.write(payload)
f.FixtureProvider.do_POST = complete
try:
    f.start()
    f.client.settimeout(120)
    assert rpc('primary_launch_probe')['enabled'] is False
    rejected = rpc('primary_launch', request={'request': uid(), 'expected_revision': 0, 'input': {'placement': {'kind': 'standalone', 'root': str(f.project)}, 'cwd': {'kind': 'existing', 'path': str(f.project)}}})
    assert rejected['response']['status'] == 'rejected' and rejected['response']['issue']['code'] == 'unsupported_capability', rejected
    assert not list((f.home / 'sessions').glob('*.json')) and (not f.posts)
    f.reader.close()
    f.client.close()
    f.proc.terminate()
    f.proc.wait(timeout=30)
    config = f.home / 'config.toml'
    config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))
    f.start()
    f.client.settimeout(120)
    assert rpc('primary_launch_probe')['enabled'] is True
    workspace('initialize', 'status', request=uid())
    project = change('create_project', name='Launch fixture')
    area = change('create_work_area', project=project['id'], name='Area')
    repo = change('create_repository', name='Repo', remotes=[])
    change('associate_repository', project=project['id'], repository=repo['id'])
    checkout_path = f.ROOT / 'checkout'
    subprocess.run(['git', 'init', '-q', str(checkout_path)], env=f.env, check=True)
    checkout = change('register_location', name='Checkout', path=str(checkout_path), registration={'kind': 'checkout', 'home': {'kind': 'work_area', 'id': area['id']}, 'repository': repo['id']})
    directory = change('register_location', name='Directory', path=str(f.project), registration={'kind': 'directory', 'home': {'kind': 'project', 'id': project['id']}})
    records = []
    for kind, identity, path in [('project', project['id'], checkout_path), ('work_area', area['id'], checkout_path), ('checkout', checkout['id'], checkout_path), ('directory', directory['id'], f.project)]:
        request = spec({'kind': 'existing', 'placement': {'kind': kind, 'id': identity}}, path)
        record = launch(request)
        assert launch(request) == record
        peer = connect(f.sockpath)
        reply = exchange(peer, {'type': 'primary_launch', 'id': 91, 'request': request}, 'id', 'primary_launch_response')
        assert reply['response']['record'] == record
        records.append(record)
    empty = f.ROOT / 'standalone'
    request = spec({'kind': 'standalone', 'root': str(empty)}, empty, True)
    records.append(launch(request))
    assert empty.is_dir() and (not list(empty.iterdir()))
    assert len({record['session'] for record in records}) == 5 and (not f.posts)
    cli = [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', '--socket', str(f.sockpath)]
    for mode in ['repl', 'run']:
        root = f.ROOT / f'{mode}-cwd'
        root.mkdir()
        request = spec({'kind': 'standalone', 'root': str(root)}, root)
        document = f.ROOT / f'{mode}-launch.json'
        document.write_text(json.dumps(request))
        args = cli + ['--primary-launch', str(document), mode] + (['--json', 'SYNTHETIC PRIMARY INPUT'] if mode == 'run' else [])
        completed = subprocess.run(args, input='quit\n' if mode == 'repl' else None, text=True, capture_output=True, env=f.env, timeout=180)
        (f.ROOT / f'{mode}.stdout').write_text(completed.stdout)
        (f.ROOT / f'{mode}.stderr').write_text(completed.stderr)
        assert completed.returncode == 0, completed.stderr
        if mode == 'run':
            assert 'Synthetic primary caller completed' in completed.stdout
    api_path = ipc / 'a.sock'
    log = open(f.ROOT / 'bridge.log', 'wb')
    bridge = subprocess.Popen(cli + ['api-bridge', '--api-socket', str(api_path)], env=f.env, stdout=log, stderr=log)
    children.append(bridge)
    deadline = time.monotonic() + 30
    while not api_path.exists():
        assert bridge.poll() is None
        if time.monotonic() > deadline:
            raise TimeoutError('bridge startup')
        time.sleep(0.05)
    api = connect(api_path)
    hello = exchange(api, {'v': 1, 'id': 1, 'req': 'hello', 'min_version': 1, 'max_version': 1, 'client': 'primary-fixture'}, 'reply_to', 'hello_ok')
    assert 'primary_launch_v1' in hello['capabilities']
    assert exchange(api, {'v': 1, 'id': 2, 'req': 'primary_launch_probe'}, 'reply_to', 'primary_launch_capabilities')['enabled']
    root = f.ROOT / 'harness-cwd'
    root.mkdir()
    request = spec({'kind': 'standalone', 'root': str(root)}, root)
    response = exchange(api, {'v': 1, 'id': 3, 'req': 'primary_launch', 'request': request}, 'reply_to', 'primary_launch')
    assert response['response']['status'] == 'launched', response
    session = response['response']['record']['session']
    exchange(api, {'v': 1, 'id': 4, 'req': 'attach_session', 'session_id': session}, 'reply_to', 'attached')
    acp_log = open(f.ROOT / 'acp.stderr', 'wb')
    acp = subprocess.Popen(cli + ['acp'], env=f.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=acp_log, bufsize=0)
    children.append(acp)

    def acp_request(identity, method, params):
        acp.stdin.write((json.dumps({'jsonrpc': '2.0', 'id': identity, 'method': method, 'params': params}) + '\n').encode())
        acp.stdin.flush()
        deadline = time.monotonic() + 180
        while True:
            if not select.select([acp.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
                raise TimeoutError('ACP reply')
            line = acp.stdout.readline()
            assert line, 'ACP closed before reply'
            event = json.loads(line)
            if event.get('id') == identity:
                assert 'error' not in event, event
                return event['result']
    acp_request(1, 'initialize', {'protocolVersion': 1, 'clientCapabilities': {}, 'clientInfo': {'name': 'primary-fixture', 'version': '1'}})
    root = f.ROOT / 'acp-cwd'
    root.mkdir()
    request = spec({'kind': 'standalone', 'root': str(root)}, root)
    acp_created = acp_request(2, 'session/new', {'cwd': str(root), 'mcpServers': [], '_meta': {'jcode_primary_launch': request}})
    assert acp_created['sessionId']
    acp.stdin.close()
    assert len(f.posts) == 1, f.posts
    result = {'source_binary': f.BIN, 'all_placements': True, 'two_client_replay': True, 'empty_cwd': True, 'run': True, 'repl': True, 'harness': True, 'acp': True, 'provider_requests': len(f.posts), 'records': records, 'root': str(f.ROOT)}
    (f.ROOT / 'primary-launch-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
    cleanup = {}
    for client, reader in channels:
        reader.close()
        client.close()
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    for process in children + ([f.proc] if f.proc else []):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        cleanup[str(process.pid)] = process.returncode
    f.http.shutdown()
    f.http.server_close()
    f.log.close()
    (f.ROOT / 'primary-launch-cleanup.json').write_text(json.dumps(cleanup, indent=2))
    (f.ROOT / 'primary-launch-events.json').write_text(json.dumps(f.events, indent=2))
    (f.ROOT / 'primary-launch-provider.json').write_text(json.dumps(f.posts, indent=2))
