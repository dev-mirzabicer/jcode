#!/usr/bin/env python3
"""Real candidate binary + daemon + CLI, using only localhost scripted inference."""
import argparse, http.server, json, os, pathlib, sqlite3, subprocess, threading, time, tempfile, signal, traceback, socket as sockets, base64
os.umask(63)
parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--evidence-parent', required=True)
args = parser.parse_args()
assert os.environ.get('JCODE_TEST_STATE_ROOT'), 'Use scripts/run_isolated_test.py'
base = pathlib.Path(args.evidence_parent)
root = pathlib.Path(tempfile.mkdtemp(prefix='wp02-native-', dir=base))
for leaf in ['home', 'state', 'runtime', 'work', 'tmp']:
    (root / leaf).mkdir()
binary = pathlib.Path(args.binary).resolve(strict=True)
requests = []
errors = []

class Handler(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({'data': [{'id': 'deepseek-v4-flash-free', 'context_length': 1000000}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        try:
            payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(payload)
            (root / f'request-{len(requests)}.json').write_text(json.dumps(payload, ensure_ascii=False))
            assert self.path.endswith('/chat/completions'), self.path
            count = len(requests)
            if count == 1:
                command = '/usr/bin/python3 -c \'from pathlib import Path; f=Path("effects").open("a"); f.write("x"); f.close(); print("x"*1000000); print("PRESSURE_TAIL")\''
                name = 'bash'
                arguments = {'command': command, 'intent': 'Native retention fixture', 'output_size': 500000}
                finish = 'tool_calls'
            elif count == 2:
                history = [str(m.get('content', '')) for m in payload['messages'] if m.get('role') == 'tool']
                assert history and any(('withheld' in text.lower() for text in history)), history
                assert not any(('PRESSURE_TAIL' in text for text in history))
                finish = 'stop'
            elif count == 3:
                assert not any((str(m.get('reasoning_content') or '').strip() for m in payload['messages'] if m.get('role') == 'assistant')), 'Explicit suppression was not reflected in provider input'
                with sqlite3.connect(root / 'state/execution/index.sqlite') as db:
                    source = db.execute("SELECT output_path FROM runs WHERE tool='bash' AND state='completed'").fetchone()[0]
                name = 'read'
                arguments = {'file_path': source, 'start_line': 2, 'end_line': 2, 'intent': 'Retrieve saved output after explicit context transaction', 'output_size': 'small'}
                finish = 'tool_calls'
            elif count == 4:
                assert any(('PRESSURE_TAIL' in str(m.get('content', '')) for m in payload['messages'] if m.get('role') == 'tool'))
                finish = 'stop'
            else:
                raise AssertionError(f'Unexpected provider replay {count}')
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Connection', 'close')
            self.end_headers()
            if count in (1, 3):
                delta = {'tool_calls': [{'index': 0, 'id': f'native-fixture-{count}', 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(arguments)}}]}
            else:
                delta = {'content': 'Native fixture complete'}
            if count == 1:
                delta['reasoning_content'] = 'SYNTHETIC REPLAYED REASONING ' * 100
            for choice in [{'delta': delta, 'finish_reason': None}, {'delta': {}, 'finish_reason': finish}]:
                self.wfile.write(('data: ' + json.dumps({'id': f'fixture-{count}', 'model': 'deepseek-v4-flash-free', 'choices': [{'index': 0, **choice}]}) + '\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
            self.close_connection = True
        except Exception:
            errors.append(traceback.format_exc())
            (root / 'provider-errors.txt').write_text('\n'.join(errors))
            try:
                self.send_error(500, 'Fixture assertion failed')
            except Exception:
                pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
config = f'[features]\nswarm = false\nmemory = false\n[sponsors]\nenabled = false\n[providers.fixture]\ntype = "openai-compatible"\nbase_url = "http://127.0.0.1:{server.server_port}/v1"\nauth = "none"\ndefault_model = "deepseek-v4-flash-free"\nmodel_catalog = false\nmodels = [{{id = "deepseek-v4-flash-free", context_window = 1000000, input = ["text"]}}]\n'
(root / 'state/config.toml').write_text(config)
env = {'PATH': os.environ['PATH'], 'HOME': str(root / 'home'), 'USER': os.environ.get('USER', 'mirzabicer'), 'LANG': 'en_US.UTF-8', 'JCODE_HOME': str(root / 'state'), 'JCODE_RUNTIME_DIR': os.environ['JCODE_RUNTIME_DIR'], 'TMPDIR': str(root / 'tmp'), 'JCODE_SCRATCH_DIR': str(root / 'tmp'), 'JCODE_NO_TELEMETRY': '1', 'JCODE_NO_BROWSER': '1', 'NO_BROWSER': '1', 'JCODE_TEST_SESSION': '1', 'JCODE_SWARM_ENABLED': 'false', 'JCODE_MEMORY_ENABLED': 'false'}
socket = pathlib.Path(os.environ['JCODE_RUNTIME_DIR']) / 'pressure.sock'
flags = [str(binary), '--no-update', '--no-selfdev', '--provider-profile', 'fixture', '--model', 'deepseek-v4-flash-free', '--socket', str(socket), '-C', str(root / 'work')]
result = {'root': str(root), 'binary': str(binary), 'status': 'incomplete'}
daemon = None
bridge = None
connection = None
try:
    result['version'] = subprocess.check_output([str(binary), '--version'], env=env, text=True).strip()
    with (root / 'daemon.log').open('wb') as log:
        daemon = subprocess.Popen(flags + ['serve', '--temporary-server', '--owner-pid', str(os.getpid()), '--temp-idle-timeout-secs', '120'], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    deadline = time.monotonic() + 30
    while not socket.exists():
        if daemon.poll() is not None:
            raise RuntimeError(f'Daemon exited {daemon.returncode}')
        if time.monotonic() > deadline:
            raise TimeoutError('Daemon did not publish its socket')
        time.sleep(0.05)
    api_socket = pathlib.Path(os.environ['JCODE_RUNTIME_DIR']) / 'pressure-api.sock'
    with (root / 'bridge.log').open('wb') as log:
        bridge = subprocess.Popen(flags + ['api-bridge', '--api-socket', str(api_socket)], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    deadline = time.monotonic() + 30
    while not api_socket.exists():
        if bridge.poll() is not None:
            raise RuntimeError(f'Bridge exited {bridge.returncode}')
        if time.monotonic() > deadline:
            raise TimeoutError('API bridge did not publish its socket')
        time.sleep(0.05)
    connection = sockets.socket(sockets.AF_UNIX, sockets.SOCK_STREAM)
    connection.settimeout(30)
    connection.connect(str(api_socket))
    wire = connection.makefile('rwb', buffering=0)
    sequence = 0
    events = []

    def api(req, **arguments):
        global sequence
        sequence += 1
        request_id = sequence
        wire.write((json.dumps({'v': 1, 'id': request_id, 'req': req, **arguments}) + '\n').encode())
        while True:
            line = wire.readline()
            if not line:
                raise EOFError('API bridge closed before reply')
            frame = json.loads(line)
            events.append(frame)
            (root / 'api-events.json').write_text(json.dumps(events, ensure_ascii=False))
            if frame.get('reply_to') == request_id:
                if frame.get('ev') == 'error':
                    raise RuntimeError(frame)
                return frame
    hello = api('hello', min_version=1, max_version=1, client='wp02-native-fixture')
    assert 'shared_execution_v1' in hello['capabilities'] and 'shared_execution_parts_v1' in hello['capabilities'], hello
    attached = api('create_session', working_dir=str(root / 'work'))
    session = attached['session']['session_id']
    wire.write((json.dumps({'v': 1, 'req': 'send_message', 'session_id': session, 'content': 'Exercise the native retention fixture.'}) + '\n').encode())
    while not any((e.get('ev') == 'turn_done' for e in events)):
        events.append(json.loads(wire.readline()))
        (root / 'api-events.json').write_text(json.dumps(events, ensure_ascii=False))
    assert len(requests) == 2 and (not errors), (len(requests), errors)
    assert any((m.get('reasoning_content') for m in requests[1]['messages'] if m.get('role') == 'assistant')), 'Fixture must prove initial reasoning replay'
    legacy = sockets.socket(sockets.AF_UNIX, sockets.SOCK_STREAM)
    legacy.settimeout(15)
    legacy.connect(str(socket))
    control = legacy.makefile('rwb', buffering=0)
    context_events = []

    def receive_until(kind, request_id):
        while True:
            frame = json.loads(control.readline())
            context_events.append(frame)
            (root / 'context-events.json').write_text(json.dumps(context_events, ensure_ascii=False))
            if frame.get('type') in ['error', 'context_draft_failed', 'context_draft_stale']:
                raise RuntimeError(frame)
            if frame.get('type') == kind and frame.get('id') == request_id:
                return frame
    control.write((json.dumps({'type': 'subscribe', 'id': 100, 'target_session_id': session, 'working_dir': str(root / 'work')}) + '\n').encode())
    control.write((json.dumps({'type': 'prepare_context_draft', 'id': 101, 'request': {'reasoning': {'kind': 'keep_latest_assistant_turns', 'protected_recent_assistant_turns': 0}, 'authorization': {'kind': 'manual', 'initiated_by': 'isolated-native-acceptance-client'}}}) + '\n').encode())
    ready = receive_until('context_draft_ready', 101)
    draft = ready['draft']
    draft_id = draft['identity']['draft_id']
    assert draft['required_operations'], draft
    control.write((json.dumps({'type': 'apply_context_draft', 'id': 102, 'draft_id': draft_id}) + '\n').encode())
    applied = receive_until('context_transaction_applied', 102)['result']
    assert applied['revision'] > draft['identity']['base_context_revision']
    control.close()
    legacy.close()
    wire.write((json.dumps({'v': 1, 'req': 'send_message', 'session_id': session, 'content': 'Retrieve the retained result after the explicit context edit.'}) + '\n').encode())
    while sum((e.get('ev') == 'turn_done' for e in events)) < 2:
        events.append(json.loads(wire.readline()))
        (root / 'api-events.json').write_text(json.dumps(events, ensure_ascii=False))
    assert len(requests) == 4 and (not errors), (len(requests), errors)
    with sqlite3.connect(root / 'state/execution/index.sqlite') as db:
        rows = db.execute("SELECT output_path FROM runs WHERE tool='bash'").fetchall()
    assert len(rows) == 1 and (root / 'work/effects').read_bytes() == b'x'
    saved = pathlib.Path(rows[0][0]).read_bytes()
    assert saved == b'x' * 1000000 + b'\nPRESSURE_TAIL\n', len(saved)
    (root / 'api-events.json').write_text(json.dumps(events, ensure_ascii=False))
    result.update(status='passed', provider_requests=len(requests), session=session, context_revision=applied['revision'], draft_id=draft_id, effects=1, retained_bytes=len(saved), withheld_then_explicit_context_edit_then_read=True)
except Exception:
    result.update(status='failed', error=traceback.format_exc(), provider_requests=len(requests))
finally:
    if connection:
        connection.close()
    if bridge and bridge.poll() is None:
        os.killpg(bridge.pid, signal.SIGTERM)
        try:
            bridge.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(bridge.pid, signal.SIGKILL)
            bridge.wait()
    if daemon and daemon.poll() is None:
        os.killpg(daemon.pid, signal.SIGTERM)
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(daemon.pid, signal.SIGKILL)
            daemon.wait()
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
    (root / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
raise SystemExit(0 if result['status'] == 'passed' else 1)
