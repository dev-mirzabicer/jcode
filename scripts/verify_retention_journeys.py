#!/usr/bin/env python3
"""WP03 actual-binary tools, snapshots, restart, automatic retention and cleanup.
All inference is scripted localhost data. Clock aging is restricted to this fixture.
"""
import argparse, base64, http.server, json, os, pathlib, plistlib, re, signal, socket, sqlite3, subprocess, sys, tempfile, threading, time, traceback
os.umask(63)
parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--evidence-parent', required=True)
parser.add_argument('--mode', choices=['daemon', 'json', 'ndjson', 'repl'], default='daemon')
args = parser.parse_args()
assert os.environ.get('JCODE_TEST_STATE_ROOT'), 'Use scripts/run_isolated_test.py'
base = pathlib.Path(args.evidence_parent)
root = pathlib.Path(tempfile.mkdtemp(prefix='wp03-native-', dir=base))
for name in ('home', 'state', 'runtime', 'work', 'tmp'):
    (root / name).mkdir()
binary = pathlib.Path(args.binary).resolve(strict=True)
mode = args.mode
assert mode in ('daemon', 'json', 'ndjson', 'repl')
archive_name = 'jcode-execution-fixture-' + root.name
archive_root = pathlib.Path('/Volumes/Active') / archive_name
assert not archive_root.exists()
requests = []
errors = []
observed = {}
processes = []
connection = None
wire = None
result = {'root': str(root), 'binary': str(binary), 'mode': mode, 'status': 'incomplete'}
signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt('Fixture stopped')))

def text(value):
    if isinstance(value, str):
        return value
    return ''.join((item.get('text', '') for item in value or [] if isinstance(item, dict)))

def body(value):
    value = text(value)
    return json.JSONDecoder().raw_decode(value[value.index('{'):])[0]

class Provider(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        data = json.dumps({'data': [{'id': 'fixture-model', 'context_length': 1000000}]}).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        try:
            payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(payload)
            n = len(requests)
            (root / f'provider-{n}.json').write_text(json.dumps(payload, ensure_ascii=False))
            tools = {item['function']['name'] for item in payload.get('tools', [])}
            assert {'session_outline', 'read_transcript', 'expand_tool_use'} <= tools, tools
            assert not {'swarm', 'memory', 'output_cleanup'} & tools
            history = [m for m in payload['messages'] if m.get('role') == 'tool']
            if n == 1:
                name = 'bash'
                args = {'command': '/usr/bin/python3 -c \'from pathlib import Path; f=Path("effects").open("a"); f.write("x"); f.close(); print("x"*50000+"WP03_NATIVE_TAIL")\'', 'intent': 'Produce one native fixture result', 'output_size': 1000}
            elif n == 2:
                name = 'session_outline'
                args = {'target': 'self', 'intent': 'Capture exact native fixture history', 'output_size': 'very_large'}
            elif n == 3:
                outline = body(history[-1]['content'])
                observed['snapshot'] = outline['snapshot_id']
                observed['tool'] = next((item['tool_use_id'] for item in outline['tools'] if item['name'] == 'bash'))
                observed['message_count'] = outline['message_count']
                name = 'read_transcript'
                args = {'snapshot_id': observed['snapshot'], 'raw': True, 'intent': 'Read the captured original history', 'output_size': 200000}
            elif n == 4:
                transcript = body(history[-1]['content'])
                assert len(transcript['messages']) == observed['message_count']
                name = 'expand_tool_use'
                args = {'snapshot_id': observed['snapshot'], 'tool_use_id': observed['tool'], 'intent': 'Read full original output without repeating it', 'output_size': 100000}
            elif n == 5:
                expanded = body(history[-1]['content'])
                assert expanded['retained_output'] == 'x' * 50000 + 'WP03_NATIVE_TAIL\n'
            else:
                raise AssertionError('Unexpected provider replay')
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Connection', 'close')
            self.end_headers()
            delta = {'content': 'WP03_NATIVE_DONE'} if n == 5 else {'tool_calls': [{'index': 0, 'id': f'wp03-call-{n}', 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(args)}}]}
            for item in ({'delta': delta, 'finish_reason': None}, {'delta': {}, 'finish_reason': 'stop' if n == 5 else 'tool_calls'}):
                self.wfile.write(('data: ' + json.dumps({'id': f'fixture-{n}', 'model': 'fixture-model', 'choices': [{'index': 0, **item}]}) + '\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
            self.close_connection = True
        except BaseException:
            errors.append(traceback.format_exc())
            (root / 'provider-errors.txt').write_text('\n'.join(errors))
            try:
                self.send_error(500, 'Fixture failed')
            except Exception:
                pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
config = f'[features]\nswarm=false\nmemory=false\n[sponsors]\nenabled=false\n[providers.fixture]\ntype="openai-compatible"\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nauth="none"\ndefault_model="fixture-model"\nmodel_catalog=false\nmodels=[{{id="fixture-model",context_window=1000000,input=["text"]}}]\n[output.storage]\nlocal_reserve_bytes=1073741824\narchive_reserve_bytes=1073741824\n[output.storage.archive]\nmount="/Volumes/Active"\nvolume_uuid="5B3BF7CE-D42A-432A-80C6-A7279A89DC77"\ndirectory="{archive_name}"\n'
(root / 'state/config.toml').write_text(config)
env = {'PATH': os.environ['PATH'], 'HOME': str(root / 'home'), 'USER': os.environ.get('USER', 'mirzabicer'), 'LANG': 'en_US.UTF-8', 'JCODE_HOME': str(root / 'state'), 'JCODE_RUNTIME_DIR': os.environ['JCODE_RUNTIME_DIR'], 'TMPDIR': str(root / 'tmp'), 'JCODE_SCRATCH_DIR': str(root / 'tmp'), 'JCODE_NO_TELEMETRY': '1', 'JCODE_NO_BROWSER': '1', 'NO_BROWSER': '1', 'JCODE_TEST_SESSION': '1', 'JCODE_SWARM_ENABLED': 'false', 'JCODE_MEMORY_ENABLED': 'false'}
flags = [str(binary), '--no-update', '--no-selfdev', '--provider-profile', 'fixture', '--model', 'fixture-model', '-C', str(root / 'work')]
main_socket = pathlib.Path(os.environ['JCODE_RUNTIME_DIR']) / 'retention.sock'
api_socket = pathlib.Path(os.environ['JCODE_RUNTIME_DIR']) / 'retention-api.sock'
sequence = 0

def sql(query, args=()):
    with sqlite3.connect(root / 'state/execution/index.sqlite', timeout=10) as db:
        return db.execute(query, args).fetchall()

def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)

def start_process(args, label, path):
    if path.exists():
        import stat
        assert path.parent == pathlib.Path(os.environ['JCODE_RUNTIME_DIR']) and stat.S_ISSOCK(path.lstat().st_mode)
        path.unlink()
    with (root / label).open('ab') as log:
        process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    processes.append(process)
    deadline = time.monotonic() + 40
    while not path.exists():
        assert process.poll() is None, (label, process.returncode)
        if time.monotonic() > deadline:
            raise TimeoutError(label)
        time.sleep(0.05)
    return process

def connect():
    global connection, wire
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.settimeout(20)
    connection.connect(str(api_socket))
    wire = connection.makefile('rwb', buffering=0)

def receive():
    line = wire.readline()
    if not line:
        raise EOFError('Fixture API closed')
    event = json.loads(line)
    with (root / 'events.jsonl').open('a') as log:
        log.write(json.dumps(event, ensure_ascii=False) + '\n')
    return event

def api(req, allow_error=False, **args):
    global sequence
    sequence += 1
    identifier = sequence
    deadline = time.monotonic() + 30
    wire.write((json.dumps({'v': 1, 'id': identifier, 'req': req, **args}) + '\n').encode())
    while time.monotonic() < deadline:
        event = receive()
        if event.get('reply_to') == identifier:
            if not allow_error:
                assert event.get('ev') != 'error', event
            return event
    raise TimeoutError(req)

def inspect(session, request, allow_error=False):
    return api('session_inspection', allow_error=allow_error, session_id=session, request=request)

def close_connection():
    global connection, wire
    if wire:
        wire.close()
        wire = None
    if connection:
        connection.close()
        connection = None
try:
    result['version'] = subprocess.check_output([str(binary), '--version'], env=env, text=True).strip()
    if mode != 'daemon':
        arguments = ['repl'] if mode == 'repl' else ['run', '--' + mode, 'Exercise the WP03 isolated native fixture.']
        process = subprocess.Popen(flags + arguments, env=env, stdin=subprocess.PIPE if mode == 'repl' else subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        processes.append(process)
        stdout, stderr = process.communicate(b'Exercise the WP03 native fixture.\nexit\n' if mode == 'repl' else None, timeout=150)
        (root / 'stdout').write_bytes(stdout)
        (root / 'stderr').write_bytes(stderr)
        assert process.returncode == 0, stderr.decode(errors='replace')[-2000:]
        parsed = stdout.decode() if mode == 'repl' else json.loads(stdout) if mode == 'json' else [json.loads(line) for line in stdout.splitlines() if line]
        assert 'WP03_NATIVE_DONE' in json.dumps(parsed)
    else:
        daemon = start_process(flags + ['--socket', str(main_socket), 'serve', '--temporary-server', '--owner-pid', str(os.getpid()), '--temp-idle-timeout-secs', '300'], 'daemon.log', main_socket)
        bridge = start_process(flags + ['--socket', str(main_socket), 'api-bridge', '--api-socket', str(api_socket)], 'bridge.log', api_socket)
        connect()
        hello = api('hello', min_version=1, max_version=1, client='wp03-native')
        assert {'session_inspection_v1', 'output_cleanup_review_v1'} <= set(hello['capabilities'])
        session = api('create_session', working_dir=str(root / 'work'))['session']['session_id']
        result['session'] = session
        wire.write((json.dumps({'v': 1, 'req': 'send_message', 'session_id': session, 'content': 'Exercise the WP03 native fixture.'}) + '\n').encode())
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if receive().get('ev') == 'turn_done':
                break
        else:
            raise TimeoutError('Native tool turn')
        assert len(requests) == 5 and (not errors), (len(requests), errors)
        bash_run = next((run for run in api('execution', session_id=session, request={'action': 'list', 'limit': 200})['response']['runs'] if run['tool'] == 'bash'))
        captured = []
        for _ in range(3):
            captured.append(inspect(session, {'action': 'outline', 'target': 'self', 'output_size': 200000})['response']['snapshot_id'])
        stable = captured[-1]
        before = body(inspect(session, {'action': 'transcript', 'snapshot_id': stable, 'raw': True, 'output_size': 200000})['response']['content']['output'])
        api('rewind', session_id=session, message_index=1)
        assert body(inspect(session, {'action': 'transcript', 'snapshot_id': stable, 'raw': True, 'output_size': 200000})['response']['content']['output']) == before
        close_connection()
        stop(bridge)
        stop(daemon)
        daemon = start_process(flags + ['--socket', str(main_socket), 'serve', '--temporary-server', '--owner-pid', str(os.getpid()), '--temp-idle-timeout-secs', '300'], 'daemon-restart.log', main_socket)
        bridge = start_process(flags + ['--socket', str(main_socket), 'api-bridge', '--api-socket', str(api_socket)], 'bridge-restart.log', api_socket)
        connect()
        api('hello', min_version=1, max_version=1, client='wp03-native-restart')
        api('attach_session', session_id=session)
        assert body(inspect(session, {'action': 'transcript', 'snapshot_id': stable, 'raw': True, 'output_size': 200000})['response']['content']['output']) == before
        deadline = time.monotonic() + 10
        while sql('SELECT count(*) FROM session_activity_leases WHERE session_id=?', (session,))[0][0]:
            if time.monotonic() > deadline:
                raise TimeoutError('Live activity lease remained after turn')
            time.sleep(0.05)
        aged = int(time.time()) - 8 * 86400
        sql('UPDATE session_activity SET last_active=?,retention_not_before=0 WHERE session_id=?', (aged, session))
        deadline = time.monotonic() + 100
        while time.monotonic() < deadline:
            status = api('output_cleanup', session_id=session, request={'action': 'status'})['response']['status']
            assert sql('SELECT last_active FROM session_activity WHERE session_id=?', (session,))[0][0] == aged, 'Polling refreshed activity'
            archived = sql('SELECT archived,cold_archived_at,physical FROM output_locations WHERE id=?', (bash_run['id'],))[0]
            retained = sql("SELECT count(*) FROM inspection_snapshots WHERE reader=? AND state='retained'", (session,))[0][0]
            if archived[0] and archived[1] is not None and (retained == 2):
                break
            time.sleep(1)
        else:
            raise TimeoutError('Automatic retention did not converge')
        result['automatic_retention'] = status
        result['retained_snapshots'] = retained
        assert pathlib.Path(archived[2]).is_relative_to(archive_root)
        assert pathlib.Path(bash_run['output_path']).read_text() == 'x' * 50000 + 'WP03_NATIVE_TAIL\n'
        assert inspect(session, {'action': 'transcript', 'snapshot_id': observed['snapshot']}, True)['ev'] == 'error'
        assert body(inspect(session, {'action': 'transcript', 'snapshot_id': stable, 'raw': True, 'output_size': 200000})['response']['content']['output']) == before
        preview = api('output_cleanup', session_id=session, request={'action': 'review', 'selection': {'selection': 'outputs', 'run_ids': [bash_run['id']]}})['response']['review']
        assert preview['candidates'][0]['run_id'] == bash_run['id'] and stable in preview['candidates'][0]['affected_snapshot_ids']
        assert api('output_cleanup', allow_error=True, session_id=session, request={'action': 'confirm', 'review_id': preview['review_id'], 'confirmation_id': 'wrong'})['ev'] == 'error'
        deleted = api('output_cleanup', session_id=session, request={'action': 'confirm', 'review_id': preview['review_id'], 'confirmation_id': preview['confirmation_id']})['response']['outcome']
        assert deleted['items'][0]['deleted']
        assert api('output_cleanup', session_id=session, request={'action': 'confirm', 'review_id': preview['review_id'], 'confirmation_id': preview['confirmation_id']})['response']['outcome'] == deleted
        failed = inspect(session, {'action': 'expand_tool', 'snapshot_id': stable, 'tool_use_id': observed['tool']}, True)
        assert failed['ev'] == 'error' and 'deliberately' in failed['message'], failed
        assert len(requests) == 5 and (not errors), 'Inspection/restart/maintenance repeated inference'
    assert len(requests) == 5 and (not errors), (len(requests), errors)
    assert (root / 'work/effects').read_bytes() == b'x', 'Original effect repeated'
    result.update(status='passed', provider_requests=len(requests), effects=1, tool_snapshot=observed)
except BaseException:
    result.update(status='failed', error=traceback.format_exc(), provider_requests=len(requests))
finally:
    close_connection()
    for process in reversed(processes):
        try:
            stop(process)
        except Exception:
            result.setdefault('cleanup_errors', []).append(traceback.format_exc())
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
    if archive_root.exists():
        try:
            info = plistlib.loads(subprocess.check_output(['/usr/sbin/diskutil', 'info', '-plist', '/Volumes/Active']))
            assert info['VolumeUUID'] == '5B3BF7CE-D42A-432A-80C6-A7279A89DC77'
            assert archive_root.parent == pathlib.Path('/Volumes/Active') and (not archive_root.is_symlink())
            import shutil
            shutil.rmtree(archive_root)
        except Exception:
            result.setdefault('cleanup_errors', []).append(traceback.format_exc())
    if result.get('cleanup_errors'):
        result['status'] = 'failed'
    (root / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
sys.exit(0 if result['status'] == 'passed' else 1)
