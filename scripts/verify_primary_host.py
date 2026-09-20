#!/usr/bin/env python3
"""Owned ordinary-daemon acceptance. Only localhost scripted provider traffic."""
import argparse, http.server, json, os, pathlib, socket, sqlite3, subprocess, tempfile, threading, time, traceback
os.umask(63)
p = argparse.ArgumentParser()
p.add_argument('--binary', required=True)
p.add_argument('--evidence-parent', required=True)
p.add_argument('--detach-seconds', type=int, default=330)
a = p.parse_args()
assert os.environ.get('JCODE_TEST_STATE_ROOT'), 'Use scripts/run_isolated_test.py'
assert a.detach_seconds > 300, 'Ordinary lifetime acceptance must exceed five minutes'
base = pathlib.Path(a.evidence_parent)
base.mkdir(parents=True, exist_ok=True)
root = pathlib.Path(tempfile.mkdtemp(prefix='host-', dir=base))
for name in ['home', 'state', 'work', 'other', 'tmp']:
    (root / name).mkdir()
runtime = pathlib.Path(tempfile.mkdtemp(prefix='ph-', dir=os.environ['JCODE_RUNTIME_DIR']))
endpoint = runtime / 's.sock'
binary = pathlib.Path(a.binary).resolve(strict=True)
requests = []
failures = []
clients = []
daemon = None
fixture = root / 'native.py'
fixture.write_text('from pathlib import Path\nimport os,sys,time\nmode=sys.argv[1]\nroot=Path(sys.argv[2])\n(root/(mode+"-pid")).write_text(str(os.getpid()))\n(root/(mode+"-cwd")).write_text(os.getcwd())\nwith (root/(mode+"-effects")).open("a") as f: f.write("x")\nprint("OWNED_PREFIX_"+mode,flush=True)\ndeadline=time.monotonic()+420\nwhile not (root/(mode+"-release")).exists():\n if time.monotonic()>deadline: raise RuntimeError("fixture release deadline")\n time.sleep(.1)\n(root/(mode+"-done")).write_text("done")\nprint("OWNED_TAIL_"+mode,flush=True)\n')

class Provider(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({'data': [{'id': 'fixture', 'context_length': 1000000}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(body)
            (root / f'provider-{len(requests)}.json').write_text(json.dumps(body))
            assert self.path.endswith('/chat/completions'), self.path
            users = [str(m.get('content', '')) for m in body['messages'] if m['role'] == 'user']
            mode = 'stop' if any(('native-stop-fixture' in s for s in users)) else 'detach'
            tools = [m for m in body['messages'] if m['role'] == 'tool']
            if not tools or (mode == 'stop' and (not any(('OWNED_PREFIX_stop' in str(m.get('content', '')) for m in tools)))):
                import shlex
                cmd = f'/usr/bin/python3 {shlex.quote(str(fixture))} {mode} {shlex.quote(str(root))}'
                delta = {'tool_calls': [{'index': 0, 'id': f'owned-{mode}', 'type': 'function', 'function': {'name': 'bash', 'arguments': json.dumps({'command': cmd, 'intent': 'Owned primary lifecycle fixture', 'run_in_background': False, 'timeout': 450000, 'output_size': 'small'})}}]}
                finish = 'tool_calls'
            else:
                assert mode == 'detach', 'Stopped work must not be automatically repeated'
                assert any(('OWNED_TAIL_detach' in str(m.get('content', '')) for m in tools))
                delta = {'content': 'Detached fixture completed exactly once'}
                finish = 'stop'
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Connection', 'close')
            self.end_headers()
            for data in [{'delta': delta, 'finish_reason': None}, {'delta': {}, 'finish_reason': finish}]:
                self.wfile.write(('data: ' + json.dumps({'id': f'fixture-{len(requests)}', 'model': 'fixture', 'choices': [{'index': 0, **data}]}) + '\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
            self.close_connection = True
        except Exception:
            failures.append(traceback.format_exc())
            (root / 'provider-errors.txt').write_text('\n'.join(failures))
            try:
                self.send_error(500, 'Fixture failed')
            except OSError:
                pass
http = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=http.serve_forever, daemon=True).start()
(root / 'state/config.toml').write_text(f'[features]\nmemory=false\nswarm=false\n[ambient]\nenabled=false\n[sponsors]\nenabled=false\n[display]\ndebug_socket=false\n[providers.fixture]\ntype="openai-compatible"\nbase_url="http://127.0.0.1:{http.server_port}/v1"\nauth="none"\ndefault_model="fixture"\nmodel_catalog=false\nmodels=[{{id="fixture",context_window=1000000,input=["text"]}}]\n')
env = {'PATH': os.environ['PATH'], 'HOME': str(root / 'home'), 'USER': os.environ.get('USER', 'mirzabicer'), 'LANG': 'en_US.UTF-8', 'TERM': 'xterm-256color', 'JCODE_HOME': str(root / 'state'), 'JCODE_RUNTIME_DIR': str(runtime), 'JCODE_SOCKET': str(endpoint), 'TMPDIR': str(root / 'tmp'), 'JCODE_SCRATCH_DIR': str(root / 'tmp'), 'JCODE_NO_TELEMETRY': '1', 'JCODE_NO_BROWSER': '1', 'NO_BROWSER': '1', 'JCODE_MEMORY_ENABLED': 'false', 'JCODE_SWARM_ENABLED': 'false'}
flags = [str(binary), '--no-update', '--no-selfdev', '--provider-profile', 'fixture', '--model', 'fixture', '--socket', str(endpoint), '-C', str(root / 'work')]

def wait_for(predicate, seconds=30):
    deadline = time.monotonic() + seconds
    while not predicate():
        assert not failures, failures
        if daemon and daemon.poll() is not None:
            raise RuntimeError('owned daemon exited')
        if time.monotonic() > deadline:
            raise TimeoutError('fixture condition deadline')
        time.sleep(0.05)

def process_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False

class Client:

    def __init__(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(40)
        self.sock.connect(str(endpoint))
        self.wire = self.sock.makefile('rwb', buffering=0)
        self.events = []
        self.counter = 0
        clients.append(self)
        self.send('primary_stream_subscribe')
        assert self.until(lambda e: e.get('type') == 'primary_stream_capabilities')['version'] == 1

    def send(self, kind, **kw):
        self.counter += 1
        self.wire.write((json.dumps({'type': kind, 'id': self.counter, **kw}) + '\n').encode())
        return self.counter

    def until(self, predicate):
        while True:
            line = self.wire.readline()
            if not line:
                raise EOFError('owned connection closed')
            event = json.loads(line)
            self.events.append(event)
            if event.get('type') == 'error':
                raise RuntimeError(event)
            if predicate(event):
                return event

    def subscribe(self, target=None, cwd=None):
        args = {'working_dir': str(cwd or root / 'work'), 'selfdev': False, 'client_has_local_history': False}
        if target:
            args['target_session_id'] = target
        else:
            args['agent'] = 'global:jcode'
        rid = self.send('subscribe', **args)
        self.until(lambda e: e.get('type') == 'done' and e.get('id') == rid and ('primary_stream' not in e))
        return [e['session_id'] for e in self.events if e.get('type') in ('session', 'history')][-1]

    def close(self):
        if self in clients:
            clients.remove(self)
        (root / f'events-{id(self)}.json').write_text(json.dumps(self.events))
        self.wire.close()
        self.sock.close()
result = {'status': 'incomplete', 'binary': str(binary), 'root': str(root), 'runtime': str(runtime), 'ordinary_non_debug': True}
cleanup = {}
try:
    result['version'] = subprocess.check_output([str(binary), '--version'], text=True, env=env).strip()
    with (root / 'daemon.log').open('wb') as log:
        daemon = subprocess.Popen(flags + ['serve', '--server-name', 'primary-fixture'], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    wait_for(endpoint.exists)
    def_debug = socket.socket(socket.AF_UNIX)
    def_debug.settimeout(10)
    def_debug.connect(str(runtime / 's-debug.sock'))
    with def_debug.makefile('rwb', buffering=0) as diagnostic:
        diagnostic.write((json.dumps({'type': 'debug_command', 'id': 1, 'command': 'server:info'}) + '\n').encode())
        response = json.loads(diagnostic.readline())
        while response.get('type') == 'ack':
            response = json.loads(diagnostic.readline())
        assert response.get('type') == 'error' and 'Debug control is disabled' in response.get('message', ''), response
        result['debug_control_rejection'] = response
        assert 'JCODE_DEBUG_CONTROL' not in env and (not (root / 'state/debug_control').exists())
    def_debug.close()
    first = Client()
    session = first.subscribe()
    result['session'] = session
    first.send('message', content='native-detach-fixture')
    wait_for(lambda: (root / 'detach-effects').exists())
    second = Client()
    peer = second.subscribe(cwd=root / 'other')
    assert peer != session
    assert first.subscribe(peer, cwd=root / 'other') == peer
    assert (root / 'detach-cwd').read_text() == str((root / 'work').resolve())
    first.close()
    second.close()
    start = time.monotonic()
    while time.monotonic() - start < a.detach_seconds:
        assert daemon.poll() is None, 'Ordinary daemon died without clients'
        assert not (root / 'detach-done').exists()
        time.sleep(1)
    result['detached_seconds'] = time.monotonic() - start
    (root / 'detach-release').write_text('release')
    wait_for(lambda: len(requests) == 2 and (root / 'detach-done').exists())
    restored = Client()
    assert restored.subscribe(session, cwd=root / 'other') == session
    rid = restored.send('get_history')
    history = restored.until(lambda e: e.get('type') == 'history' and e.get('id') == rid)
    assert history['primary_stream']['phase'] == 'snapshot'
    assert any(('Detached fixture completed exactly once' in m['content'] for m in history['messages']))
    assert (root / 'detach-effects').read_text() == 'x'
    assert len(requests) == 2
    restored.send('message', content='native-stop-fixture')
    wait_for(lambda: (root / 'stop-effects').exists())
    restored.close()
    controller = Client()
    assert controller.subscribe(session, cwd=root / 'other') == session
    assert (root / 'stop-cwd').read_text() == str((root / 'work').resolve())
    controller.send('cancel')
    controller.until(lambda e: e.get('type') == 'done' and 'primary_stream' in e)
    wait_for(lambda: not process_alive(int((root / 'stop-pid').read_text())))
    assert not (root / 'stop-done').exists()
    assert (root / 'stop-effects').read_text() == 'x'
    assert len(requests) == 3 and (not failures)
    with sqlite3.connect(root / 'state/execution/index.sqlite') as db:
        runs = db.execute("SELECT state,output_path FROM runs WHERE tool='bash' ORDER BY rowid").fetchall()
    assert len(runs) == 2, runs
    assert [state for state, _ in runs] == ['completed', 'cancelled'], runs
    outputs = [pathlib.Path(path).read_text() for _, path in runs]
    assert any(('OWNED_TAIL_detach' in text for text in outputs))
    assert any(('OWNED_PREFIX_stop' in text for text in outputs))
    result.update(status='passed', provider_requests=3, independent_native_effects=2, runs=runs, retained_output=True, cwd_unchanged=True, stop_quiescent=True)
except Exception:
    result.update(status='failed', error=traceback.format_exc(), provider_requests=len(requests))
finally:
    # Release only owned fixture waiters, then prove quiescence before daemon teardown.
    for mode in ['detach', 'stop']:
        (root / f'{mode}-release').write_text('cleanup')
    for mode in ['detach', 'stop']:
        pidfile = root / f'{mode}-pid'
        if pidfile.exists():
            pid = int(pidfile.read_text())
            deadline = time.monotonic() + 10
            while process_alive(pid) and time.monotonic() < deadline:
                time.sleep(0.05)
            cleanup[mode + '_quiescent'] = not process_alive(pid)
            if process_alive(pid):
                result.update(status='failed', cleanup_error='Owned fixture worker did not settle after release')
    for client in list(clients):
        try:
            client.close()
        except Exception:
            pass
    if daemon and daemon.poll() is None:
        daemon.terminate()
        try:
            daemon.wait(timeout=20)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=10)
    cleanup['daemon_exit'] = None if daemon is None else daemon.poll()
    cleanup['provider_errors'] = failures
    http.shutdown()
    (root / 'cleanup.json').write_text(json.dumps(cleanup, indent=2))
    (root / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2), flush=True)
    if result['status'] != 'passed':
        raise SystemExit(1)
