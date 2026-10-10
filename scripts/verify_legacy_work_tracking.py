#!/usr/bin/env python3
"""Legacy work-tracking retirement acceptance on an owned daemon.

Localhost scripted provider only. A placed fixture session first runs with the
legacy gate on (as Mirza's runtime did before activation) and creates real
initiative data. The daemon restarts with the gate off. The script checks one
announced tool-set change and none afterwards, that the model's initiative calls
and the retired workflow protocol are refused, that SDK structured output and
transfer still work, and that the retained goal files are byte-identical.

Run through scripts/run_isolated_test.py with --binary <candidate>.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import hashlib, json, socket, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

UNAVAILABLE = ("Missions, initiatives and the legacy command workflows are retired while "
               "`features.legacy_work_tracking` is off.")
PANEL_TOKEN = 'PANEL-ONLY-DESCRIPTION-TOKEN'
ipc = Path(tempfile.mkdtemp(prefix='lw-', dir=os.environ['JCODE_RUNTIME_DIR']))
old_socket = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if arg == old_socket else arg for arg in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
f.env.pop('JCODE_LEGACY_WORK_TRACKING_ENABLED', None)
config = f.home / 'config.toml'
BASE_CONFIG = config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1)
root = f.ROOT / 'work'
root.mkdir()
captured, failures, channels = [], [], []
provider_lock = threading.Lock()
result = {'status': 'failed', 'binary': f.BIN, 'root': str(f.ROOT)}
bridge = None


def set_gate(enabled):
    config.write_text(BASE_CONFIG.replace('[features]', f'[features]\nlegacy_work_tracking={str(enabled).lower()}', 1))


def tool_call(name, arguments):
    return {'tool_calls': [{'index': 0, 'id': f'call-{uuid.uuid4().hex[:8]}', 'type': 'function',
                            'function': {'name': name, 'arguments': json.dumps({'intent': 'Legacy work-tracking fixture', **arguments})}}]}


def text_of(message):
    content = message.get('content')
    if isinstance(content, list):
        return '\n'.join(str(part.get('text', part)) for part in content)
    return str(content or '')


def script(request):
    """The scripted model: one decision per request, from the transcript."""
    messages = request.get('messages', [])
    last = messages[-1] if messages else {}
    # Harness notices are appended as their own messages; the latest human turn decides.
    users = [text_of(m) for m in messages if m.get('role') == 'user'
             and not text_of(m).lstrip().startswith('<system-reminder>')]
    latest = users[-1] if users else ''
    if 'STRUCTURED-FIXTURE' in latest:
        return {'content': '{"ok": true, "fixture": "structured"}'}
    if last.get('role') == 'tool':
        tool_results = sum(1 for m in messages if m.get('role') == 'tool')
        if 'ROUND-ONE' in latest and tool_results == 1:
            return tool_call('initiative', {'action': 'focus', 'id': 'synthetic-retained-goal', 'display': 'focus'})
        if 'ROUND-FOUR' in latest and tool_results_since(messages) == 1:
            return tool_call('batch', {'tool_calls': [
                {'tool': 'initiative', 'intent': 'must reject', 'action': 'list'},
                {'tool': 'todo', 'intent': 'stays available'}]})
        return {'content': 'Synthetic tool round complete'}
    if latest.rstrip().endswith('ROUND-ONE'):
        return tool_call('initiative', {'action': 'create', 'scope': 'global', 'title': 'Synthetic retained goal',
                                        'description': PANEL_TOKEN, 'next_steps': ['synthetic next step']})
    if latest.rstrip().endswith('ROUND-FOUR'):
        return tool_call('initiative', {'action': 'create', 'scope': 'global', 'title': 'Must not persist'})
    return {'content': 'Synthetic answer'}


def tool_results_since(messages):
    count = 0
    for message in reversed(messages):
        if message.get('role') == 'user':
            break
        if message.get('role') == 'tool':
            count += 1
    return count


def complete(self):
    try:
        request = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        with provider_lock:
            captured.append(request)
        delta = script(request)
        finish = 'tool_calls' if 'tool_calls' in delta else 'stop'
        chunks = [
            {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
            {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]},
        ]
        payload = (''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    except (BrokenPipeError, ConnectionResetError):
        pass
    except Exception:
        failures.append(traceback.format_exc())
f.FixtureProvider.do_POST = complete


def connect():
    client = socket.socket(socket.AF_UNIX)
    client.settimeout(120)
    client.connect(str(f.sockpath))
    reader = client.makefile('rb')
    channels.append((client, reader))
    return client, reader


def close(channel):
    channel[1].close(); channel[0].close(); channels.remove(channel)


def rpc(channel, kind, until=None, **fields):
    f.counter += 1
    request = f.counter
    channel[0].sendall((json.dumps({'type': kind, 'id': request, **fields}) + '\n').encode())
    while True:
        line = channel[1].readline()
        assert line, 'daemon closed request channel'
        event = json.loads(line)
        f.events.append(event)
        if event.get('id') == request and event.get('type') != 'ack':
            if until and event.get('type') not in until:
                continue
            return event


def ws(action, **fields):
    event = rpc(admin, 'workspace', request={'action': action, **fields})
    assert event['type'] == 'workspace_response' and event['response']['kind'] != 'error', event
    return event['response']['value']


def wait(check, label, seconds=120):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise TimeoutError(label)


def tree(path):
    out = {}
    for item in sorted(path.rglob('*')):
        if item.is_file():
            out[str(item.relative_to(path))] = hashlib.sha256(item.read_bytes()).hexdigest()
    return out


def session_file():
    return json.loads((f.home / 'sessions' / f'{session}.json').read_text())


def turn(text, expected_requests):
    """Deliver durable primary input and wait for the scripted requests to settle."""
    before = len(captured)
    input_id = str(uuid.uuid4())
    receipt = rpc(admin, 'primary_input', input={'id': input_id, 'session': session,
                                                 'delivery': 'safe_boundary', 'content': text, 'images': []})
    assert receipt['type'] == 'primary_input_receipt', receipt
    wait(lambda: len(captured) >= before + expected_requests, f'{text}: provider requests')
    def settled():
        receipt_state = rpc(admin, 'primary_input_inspect', session=session, input=input_id)
        return receipt_state['receipt']['state'] == 'committed'
    wait(settled, f'{text}: input committed')
    time.sleep(1.0)
    assert len(captured) == before + expected_requests, (text, len(captured) - before)
    return captured[before:]


def tool_names(request):
    return {tool['function']['name'] for tool in request.get('tools', [])}


def stop_daemon():
    global bridge
    for channel in list(channels):
        close(channel)
    if bridge and bridge.poll() is None:
        bridge.terminate(); bridge.wait(timeout=20)
    bridge = None
    f.reader.close(); f.client.close(); f.proc.terminate(); f.proc.wait(timeout=60)


def start_bridge():
    global bridge, api
    api = ipc / 'api.sock'
    log = (f.ROOT / 'bridge.log').open('ab')
    bridge = subprocess.Popen([f.BIN, '--no-update', '--no-selfdev', '--socket', str(f.sockpath), 'api-bridge',
                               '--api-socket', str(api)], env=f.env, stdout=log, stderr=log)
    wait(api.exists, 'bridge ready')


def sdk(program, label):
    module = (Path(__file__).resolve().parents[1] / 'sdk/typescript/dist/index.js').as_uri()
    source = (f"import assert from 'node:assert/strict';import {{JcodeClient}} from {json.dumps(module)};\n"
              f"const c=await JcodeClient.connect({{socketPath:{json.dumps(str(api))},requestTimeoutMs:120000,ensureRuntime:false}});\n"
              f"try{{{program}}}finally{{c.close();}}")
    (f.ROOT / f'{label}.mjs').write_text(source)
    # Outside the Jcode checkout, so attaching does not enable self-development tools.
    run = subprocess.run(['node', '--input-type=module', '-e', source], env=f.env, cwd=root, capture_output=True,
                         text=True, timeout=180)
    (f.ROOT / f'{label}.stdout').write_text(run.stdout); (f.ROOT / f'{label}.stderr').write_text(run.stderr)
    assert run.returncode == 0, run.stderr
    return json.loads(run.stdout.strip().splitlines()[-1])


try:
    # Phase 1: the gate on, as before activation. Real initiative data.
    set_gate(True)
    f.start(); admin = connect()
    ws('initialize', request=str(uuid.uuid4()))
    review = ws('review', expected_revision=ws('status')['revision'], change={
        'action': 'register_location', 'name': 'work', 'path': str(root), 'registration': {'kind': 'standalone'}})
    location = {'kind': 'standalone', 'id': ws('apply', request=str(uuid.uuid4()), review=review['id'])['targets'][0]['id']}
    launched = rpc(admin, 'primary_launch', request={'request': str(uuid.uuid4()), 'expected_revision': ws('status')['revision'],
        'input': {'placement': {'kind': 'existing', 'placement': location}, 'cwd': {'kind': 'existing', 'path': str(root)},
                  'agent': None, 'model': None, 'selfdev': False}})
    assert launched['response']['status'] == 'launched', launched
    session = launched['response']['record']['session']
    round_one = turn('ROUND-ONE', 3)
    assert 'initiative' in tool_names(round_one[0])
    goals = f.home / 'goals'
    retained = tree(goals)
    assert any(name.startswith('global/') for name in retained), retained
    assert any(name.startswith('sessions/') for name in retained), retained
    panel = f.home / 'side_panel' / session / 'index.json'
    assert panel.exists() and 'goal.synthetic-retained-goal' in panel.read_text()
    panel_before = panel.read_bytes()
    token_count = json.dumps(round_one[-1]).count(PANEL_TOKEN)
    frozen = session_file()['tool_set']
    assert any(tool['name'] == 'initiative' for tool in frozen['advertised'])
    assert not frozen.get('changes')
    result['retained_files'] = len(retained)
    stop_daemon()

    # Phase 2: the gate off after a restart (activation).
    set_gate(False)
    f.start(); admin = connect()
    round_two = turn('ROUND-TWO', 1)
    assert 'initiative' not in tool_names(round_two[0]), sorted(tool_names(round_two[0]))
    changes = session_file()['tool_set']['changes']
    assert [c['change'] for c in changes] == [{'kind': 'removed', 'name': 'initiative'}], changes
    assert changes[0].get('withdrawn') is True, changes
    previous = round_one[-1]['messages']
    added = round_two[0]['messages'][len(previous):]
    assert round_two[0]['messages'][:len(previous)] == previous, 'history prefix was rewritten'
    notices = [m for m in added if m.get('role') != 'user' or 'ROUND-TWO' not in text_of(m)]
    assert sum('initiative' in text_of(m) for m in notices) == 1, added
    round_three = turn('ROUND-THREE', 1)
    assert tool_names(round_three[0]) == tool_names(round_two[0])
    added = round_three[0]['messages'][len(round_two[0]['messages']):]
    assert round_three[0]['messages'][:len(round_two[0]['messages'])] == round_two[0]['messages']
    assert not any('initiative' in text_of(m) for m in added), added
    assert len(session_file()['tool_set']['changes']) == 1
    result['tool_set_change_announced_once'] = True

    # The model still calls the retired tool directly and inside batch.
    round_four = turn('ROUND-FOUR', 3)
    tool_texts = [text_of(m) for m in round_four[-1]['messages'] if m.get('role') == 'tool'][-2:]
    assert UNAVAILABLE in tool_texts[0], tool_texts[0]
    assert UNAVAILABLE in tool_texts[1] and 'todo' in tool_texts[1].lower(), tool_texts[1]
    assert json.dumps(round_four[-1]).count(PANEL_TOKEN) == token_count, 'side panel became provider context'
    assert len(session_file()['tool_set']['changes']) == 1

    # Debug tool listing and execution use the same global gate.
    debug = subprocess.run([f.BIN, 'debug', 'tools', '-S', session, '--socket', str(f.sockpath)], env=f.env,
                           capture_output=True, text=True, timeout=30)
    (f.ROOT / 'debug-tools.txt').write_text(debug.stdout + debug.stderr)
    assert debug.returncode == 0, debug.stderr
    listed = json.loads(debug.stdout[debug.stdout.index('['):])
    assert 'initiative' not in listed and 'todo' in listed, listed

    # Retired workflow protocol, first request on a fresh connection.
    for command in ({'kind': 'commit'}, {'kind': 'plan', 'goal': None}, {'kind': 'improve', 'plan_only': False, 'focus': None}):
        fresh = connect()
        reply = rpc(fresh, 'render_workflow_prompt', workflow={'kind': 'command', 'command': command})
        assert reply['type'] == 'error' and UNAVAILABLE in reply['message'], reply
        close(fresh)
    result['workflow_protocol_rejected'] = True

    # SDK structured output through the Harness bridge.
    start_bridge()
    structured = sdk(f"await c.attachSession({json.dumps(session)});const r=await c.runStructured({json.dumps(session)},'STRUCTURED-FIXTURE',{{schema:{{type:'object',required:['ok'],properties:{{ok:{{type:'boolean'}}}}}}}});"
                     "console.log(JSON.stringify({data:r.data,attempts:r.attempts.length}));", 'structured')
    assert structured == {'data': {'ok': True, 'fixture': 'structured'}, 'attempts': 1}, structured
    result['sdk_structured_output'] = True

    # Transfer stays available.
    attached = connect()
    assert rpc(attached, 'subscribe', until={'done', 'error'}, working_dir=str(root), target_session_id=session, selfdev=False)['type'] == 'done'
    sessions_before = set(p.name for p in (f.home / 'sessions').glob('*.json'))
    transferred = rpc(attached, 'transfer', until=None)
    (f.ROOT / 'transfer-reply.json').write_text(json.dumps(transferred, indent=2))
    assert transferred['type'] != 'error', transferred
    wait(lambda: set(p.name for p in (f.home / 'sessions').glob('*.json')) - sessions_before, 'transfer session')
    result['transfer'] = transferred['type']

    # Rollback and a second retirement on the same session: each change of
    # availability is one announced change, the tool returns and leaves again,
    # and the history prefix stays append-only.
    for leg, enabled in (('ROUND-FIVE', True), ('ROUND-SIX', False)):
        stop_daemon()
        set_gate(enabled)
        f.start(); admin = connect()
        previous = captured[-1]['messages']
        sent = turn(leg, 1)[0]
        assert sent['messages'][:len(previous)] == previous, f'{leg}: history prefix was rewritten'
        assert ('initiative' in tool_names(sent)) == enabled, (leg, sorted(tool_names(sent)))
        record = session_file()['tool_set']['changes']
        kinds = [(c['change']['kind'], c.get('withdrawn', False)) for c in record
                 if (c['change'].get('name') or c['change'].get('definition', {}).get('name')) == 'initiative']
        expected = [('removed', True), ('added', False)] + ([] if enabled else [('removed', True)])
        assert kinds == expected, (leg, kinds)
        assert len(record) == len(expected), (leg, record)
        notices = [m for m in sent['messages'][len(previous):] if 'initiative' in text_of(m)]
        assert len(notices) == 1, (leg, notices)
    result['rollback_and_second_retirement'] = True

    # Retained data byte-identical; the refused calls created nothing.
    assert tree(goals) == retained, 'goal files changed'
    assert panel.read_bytes() == panel_before, 'side panel state changed'
    assert not (f.home / 'memory').exists(), 'memory store reached'
    assert not failures, failures
    result['status'] = 'passed'
finally:
    try:
        stop_daemon()
    except Exception:
        result.setdefault('cleanup_error', traceback.format_exc())
    result['provider_requests'] = len(captured)
    (f.ROOT / 'captured.json').write_text(json.dumps(captured, indent=1))
    (f.ROOT / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
