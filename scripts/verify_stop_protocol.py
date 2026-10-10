#!/usr/bin/env python3
"""Stop-protocol acceptance on an owned daemon.

Localhost scripted provider only; private state through
scripts/run_isolated_test.py. With features.session_work on:

  1. Surface. A TUI-created placed session gets declare_stop and the
     session-work wording for bash/bg, both taken from the seeded managed
     resources; the session-work skill is seeded but kept out of the
     available-skills catalog; module-types.md is written. A primary_launch
     without the opt-in gets none of it; with the opt-in (advertised by the
     launch capabilities) it is activated.
  2. Corrections. A stop without a declaration while the workflow is
     unfinished is corrected twice, appended, then the turn stops without a
     declaration.
  3. Declared completion and the tool gate: one warned call, refusals, an
     outright batch refusal, the turn ending on the third refusal with the
     declaration kept; a follow-up completes implicitly.
  4. Background work: completion refused for an unfinished workflow, then for
     a running task; an unknown leave_running alias refused; a wait on the
     task accepted, and the turn ends waiting.
  5. Nonvoluntary ends: a provider refusal and a provider rejection are
     recorded, never corrected; Stop revokes the declaration made in its turn.
  6. A child with completion only declares its own completion.
  7. A reload in the middle of a correction: the continuation keeps the turn
     and its counter.

Prose is never asserted: texts are located through the seeded resources.
Run: python3 scripts/run_isolated_test.py python3 scripts/verify_stop_protocol.py --binary <candidate>
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, re, socket, sqlite3, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc = Path(tempfile.mkdtemp(prefix='sp-', dir=os.environ['JCODE_RUNTIME_DIR']))
old_socket = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if arg == old_socket else arg for arg in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
f.env.pop('JCODE_SESSION_WORK_ENABLED', None)
prefix = f.args[:f.args.index('serve')]
config = f.home / 'config.toml'
config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true\nsession_work=true', 1))
root = (f.ROOT / 'work').resolve()
root.mkdir()
notes = root / 'notes.txt'
notes.write_text('synthetic notes\n')
captured, failures, channels = [], [], []
provider_lock = threading.Lock()
held = {'reload': False}
result = {'status': 'failed', 'binary': f.BIN, 'root': str(f.ROOT)}
WORKFLOW_LINE = re.compile(r'Workflow file: (/\S+?/workflow\.md)\. ')
UNFINISHED = '- [>] build: Build it\n- [ ] check: Check it\n'
DONE = '- [x] build: Build it\n'
REOPENED = '- [x] build: Build it\n- [>] more: More work\n'
DONE_TWO = '- [x] build: Build it\n- [x] more: More work\n'


def text_of(message):
    content = message.get('content')
    if isinstance(content, list):
        return '\n'.join(str(part.get('text', part)) for part in content)
    return str(content or '')


def workflow_path(messages):
    found = [m.group(1) for message in messages for m in WORKFLOW_LINE.finditer(text_of(message))]
    return found[-1] if found else None


def call(name, arguments):
    return {'tool_calls': [{'index': 0, 'id': f'call-{uuid.uuid4().hex[:8]}', 'type': 'function',
                            'function': {'name': name, 'arguments': json.dumps({'intent': 'Stop fixture', **arguments})}}]}


def human(message):
    return message.get('role') == 'user' and not text_of(message).lstrip().startswith('<system-reminder>')


def tool_results_since_user(messages):
    count = 0
    for message in reversed(messages):
        if human(message):
            break
        if message.get('role') == 'tool':
            count += 1
    return count


def resource(path):
    """A seeded managed text, without frontmatter."""
    text = (f.home / 'instructions' / path).read_text()
    if text.startswith('---\n'):
        text = text.split('\n---\n', 1)[1]
    return text.strip()


def correction_marker():
    return resource('notifications/session-work-correction-missing.md').splitlines()[0]


def corrections(messages):
    marker = correction_marker()
    return sum(1 for m in messages if marker in text_of(m))


def script(request):
    """The scripted model. Returns a delta, or ('status', code) to reject."""
    messages = request.get('messages', [])
    tools = {tool['function']['name'] for tool in request.get('tools', [])}
    users = [text_of(m) for m in messages if human(m)]
    latest = users[-1] if users else ''
    done = tool_results_since_user(messages)
    path = workflow_path(messages)
    if not tools:
        return {'content': 'Synthetic summary.'}
    if 'subagent' not in tools and 'CHILD-TASK' in latest:
        steps = [lambda: call('write', {'file_path': path, 'content': DONE}),
                 lambda: call('declare_stop', {'action': 'complete', 'summary': 'Child done'})]
        return steps[done]() if done < len(steps) else {'content': 'CHILD DONE'}
    tag = latest.strip().split()[-1] if latest.strip() else ''
    if tag == 'C-MISSING':
        return call('write', {'file_path': path, 'content': UNFINISHED}) if done == 0 else {'content': 'Stopping here.'}
    if tag == 'C-GATE':
        steps = [lambda: call('write', {'file_path': path, 'content': DONE}),
                 lambda: call('declare_stop', {'action': 'complete', 'summary': 'Built'}),
                 lambda: call('read', {'file_path': str(notes)}),
                 lambda: call('read', {'file_path': str(notes)}),
                 lambda: call('batch', {'tool_calls': [{'tool': 'write', 'file_path': str(root / 'batch.txt'), 'content': 'x'}]}),
                 lambda: call('read', {'file_path': str(notes)})]
        return steps[done]() if done < len(steps) else {'content': 'unexpected'}
    if tag == 'C-BG':
        steps = [lambda: call('write', {'file_path': path, 'content': REOPENED}),
                 lambda: call('bash', {'command': 'sleep 300', 'run_in_background': True, 'wake': True}),
                 lambda: call('declare_stop', {'action': 'complete'}),
                 lambda: call('write', {'file_path': path, 'content': DONE_TWO}),
                 lambda: call('declare_stop', {'action': 'complete'}),
                 lambda: call('declare_stop', {'action': 'complete', 'leave_running': ['t9']}),
                 lambda: call('declare_stop', {'action': 'wait', 'condition': 'any(t1)', 'summary': 'Waiting for the build'})]
        return steps[done]() if done < len(steps) else {'content': 'Waiting for t1.'}
    if tag == 'C-REFUSAL':
        return call('write', {'file_path': path, 'content': UNFINISHED}) if done == 0 else ('refusal', 'I cannot continue.')
    if tag == 'C-FAIL':
        return ('status', 400)
    if tag == 'C-STOP':
        steps = [lambda: call('bash', {'command': 'sleep 300', 'run_in_background': True}),
                 lambda: call('declare_stop', {'action': 'wait', 'condition': 't1', 'summary': 'Waiting'})]
        if done < len(steps):
            return steps[done]()
        return ('hold', 'stop')
    if tag == 'C-RELOAD':
        if done == 0:
            return call('write', {'file_path': path, 'content': UNFINISHED})
        if corrections(messages) == 1 and not held['reload']:
            held['reload'] = True
            return ('hold', 'reload')
        return {'content': 'Stopping here.'}
    if tag == 'C-CHILD':
        if done == 0:
            return call('subagent', {'agent': 'fixture', 'model_alias': 'fixture', 'permission': 'read_only',
                                     'preset': 'fixture', 'prompt': 'CHILD-TASK', 'disable_startup_context': True})
        return {'content': 'Child finished.'}
    return {'content': 'Synthetic answer'}


def send(handler, delta, finish):
    chunks = [
        {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
        {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]},
    ]
    payload = (''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n').encode()
    handler.send_response(200)
    handler.send_header('Content-Type', 'text/event-stream')
    handler.send_header('Content-Length', str(len(payload)))
    handler.end_headers()
    handler.wfile.write(payload)


def complete(self):
    try:
        request = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        with provider_lock:
            captured.append(request)
        reply = script(request)
        if isinstance(reply, tuple) and reply[0] == 'status':
            body = json.dumps({'error': {'message': 'synthetic rejection', 'type': 'invalid_request_error'}}).encode()
            self.send_response(reply[1]); self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
            return
        if isinstance(reply, tuple) and reply[0] == 'refusal':
            send(self, {'content': reply[1]}, 'content_filter')
            return
        if isinstance(reply, tuple) and reply[0] == 'hold':
            (f.ROOT / f'holding-{reply[1]}').write_text('holding')
            limit = time.monotonic() + 120
            while not (f.ROOT / f'release-{reply[1]}').exists() and time.monotonic() < limit:
                time.sleep(0.05)
            send(self, {'content': 'late reply'}, 'stop')
            return
        send(self, reply, 'tool_calls' if 'tool_calls' in reply else 'stop')
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


def location(command):
    event = rpc(admin, 'primary_location', command=command)
    assert event['type'] == 'primary_location_response', event
    return event['response']


def wait(check, label, seconds=120):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        last = check()
        if last:
            return last
        time.sleep(0.05)
    raise TimeoutError(f'{label}: {last!r}')


def new_placed_session():
    channel = connect()
    reply = rpc(channel, 'subscribe', until={'done', 'error'}, working_dir=str(root), selfdev=False, agent='global:jcode')
    assert reply['type'] == 'done', reply
    session = [e['session_id'] for e in f.events if e.get('type') == 'session'][-1]
    proposal = location({'action': 'propose_placement', 'session': session})
    assert proposal['status'] == 'proposal', proposal
    p = proposal['proposal']
    placed = location({'action': 'place', 'request': {
        'request': str(uuid.uuid4()), 'session': session, 'working_dir': p['working_dir'],
        'expected_catalog_revision': p['catalog_revision'], 'placement': p['candidates'][p['default']]['placement']}})
    assert placed['status'] == 'state' and placed['record']['state'] == 'complete', placed
    return session, channel


def submit(session, text):
    input_id = str(uuid.uuid4())
    receipt = rpc(admin, 'primary_input', input={'id': input_id, 'session': session,
                                                 'delivery': 'safe_boundary', 'content': text, 'images': []})
    assert receipt['type'] == 'primary_input_receipt', receipt
    return input_id


def settled(session, input_id):
    state = rpc(admin, 'primary_input_inspect', session=session, input=input_id)
    return state['receipt']['state'] in ('committed', 'failed')


def turn(session, text, expected_requests=None, outcomes_after=None):
    """Submit, then wait until the turn's outcome is recorded."""
    before = len(captured)
    recorded = len(outcomes(session))
    input_id = submit(session, text)
    wait(lambda: settled(session, input_id), f'{text}: input settled')
    wait(lambda: len(outcomes(session)) >= (outcomes_after or recorded + 1), f'{text}: outcome recorded')
    time.sleep(1.0)
    if expected_requests is not None:
        assert len(captured) == before + expected_requests, (text, len(captured) - before)
    return captured[before:]


def session_file(session):
    return json.loads((f.home / 'sessions' / f'{session}.json').read_text())


def store_path():
    for candidate in (Path(f.env['JCODE_RUNTIME_DIR']) / 'durable-state' / 'session-work' / 'store.sqlite3',
                      f.home / 'state' / 'session-work' / 'store.sqlite3'):
        if candidate.exists():
            return candidate
    return None


def store_rows(query, *params):
    path = store_path()
    if path is None:
        return []
    db = sqlite3.connect(f'file:{path}?mode=ro', uri=True)
    try:
        return db.execute(query, params).fetchall()
    finally:
        db.close()


def outcomes(session):
    return [(turn, json.loads(body)) for turn, body in
            store_rows('SELECT turn, body FROM turn_outcomes WHERE session=? ORDER BY sequence', session)]


def stops(session):
    return [json.loads(body) for (body,) in store_rows('SELECT body FROM stops WHERE session=? ORDER BY number', session)]


def tool_texts(request):
    return [text_of(m) for m in request['messages'] if m.get('role') == 'tool']


def tool_def(request, name):
    return next((tool['function'] for tool in request.get('tools', []) if tool['function']['name'] == name), None)


def system_text(request):
    return '\n'.join(text_of(m) for m in request['messages'] if m.get('role') == 'system')


def stop_daemon():
    for channel in list(channels):
        close(channel)
    f.reader.close(); f.client.close(); f.proc.terminate(); f.proc.wait(timeout=60)


try:
    f.start(); admin = connect()
    seed = connect()
    assert rpc(seed, 'subscribe', until={'done', 'error'}, working_dir=str(root), selfdev=False)['type'] == 'done'
    close(seed)
    wait(lambda: (f.home / 'instructions' / 'agents').is_dir(), 'seeded instruction store')
    instructions = f.home / 'instructions'
    (instructions / 'agents/fixture.md').write_text('---\nid: fixture\nkind: agent\nname: Fixture\ndescription: Synthetic native fixture\navailability: both\n---\nSYNTHETIC PROFILE')
    (instructions / 'model-roster.toml').write_text('[aliases.fixture]\ndescription="Synthetic local alias"\nmodels=["wp09-fixture:fixture"]\n')
    (instructions / 'notifications/task-preset.fixture.md').write_text(
        '---\nid: task-preset.fixture\nkind: notification\nname: Fixture\ndescription: Synthetic preset\n---\nSYNTHETIC PRESET')
    ws('initialize', request=str(uuid.uuid4()))
    review = ws('review', expected_revision=ws('status')['revision'], change={
        'action': 'register_location', 'name': 'work', 'path': str(root), 'registration': {'kind': 'standalone'}})
    ws('apply', request=str(uuid.uuid4()), review=review['id'])

    # 1. Surface.
    skill = instructions / 'skills' / 'session-work' / 'SKILL.md'
    assert skill.is_file() and 'disable-model-invocation: true' in skill.read_text(), 'seeded person-invoked skill'
    session, channel = new_placed_session()
    binding = session_file(session)['session_work']
    assert str(skill) in binding['context_line'], binding
    module_types = f.home / 'session-work' / session / 'module-types.md'
    assert module_types.is_file() and (module_types.stat().st_mode & 0o777) == 0o400
    first = turn(session, 'C-MISSING', 4)
    declare = tool_def(first[0], 'declare_stop')
    assert declare and declare['description'] == resource('tools/declare-stop.md'), declare
    assert declare['parameters']['properties']['action']['enum'] == ['wait', 'complete', 'withdraw']
    wake = resource('notifications/session-work-background-wake.md')
    for name in ('bash', 'bg', 'subagent'):
        assert tool_def(first[0], name)['parameters']['properties']['wake']['description'] == wake, name
    assert '/session-work' not in system_text(first[0]), 'the skill is not in the catalog'
    result['surface'] = True

    # 2. Corrections: two, appended, then stopped without a declaration.
    assert [corrections(r['messages']) for r in first] == [0, 0, 1, 2], [corrections(r['messages']) for r in first]
    for earlier, later in zip(first, first[1:]):
        assert later['messages'][:len(earlier['messages']) - 1] == earlier['messages'][:-1], 'append-only'
        assert later['tools'] == earlier['tools'], 'tool set unchanged'
    assert outcomes(session)[-1][1] == {'outcome': 'stopped_without_declaration', 'cause': 'missing'}, outcomes(session)
    result['corrections'] = True

    # 3. Declared completion and the gate.
    gate = turn(session, 'C-GATE', 6)
    results = tool_texts(gate[-1])
    warned = resource('notifications/session-work-gate-warned.md')
    refused = resource('notifications/session-work-gate-refused.md')
    batch = resource('notifications/session-work-gate-refused-batch.md')
    # The last request carries every result but the final refusal's.
    assert warned in results[-3], results[-3]
    assert refused in results[-2] and batch in results[-1], results[-2:]
    assert not (root / 'batch.txt').exists(), 'no part of a refused batch ran'
    log = json.dumps(session_file(session)['messages'])
    assert log.count(refused) >= 2, 'the third refusal is recorded too'
    assert outcomes(session)[-1][1] == {'outcome': 'completed', 'declared': True}, outcomes(session)
    completion = stops(session)[-1]
    assert completion['state'] == {'state': 'effective'}, completion
    turn(session, 'C-FOLLOW', 1)
    assert outcomes(session)[-1][1] == {'outcome': 'completed', 'declared': False}
    result['gate'] = True

    # 4. Background work.
    bg = turn(session, 'C-BG', 8)
    texts = tool_texts(bg[-1])
    unfinished = resource('notifications/session-work-stop-refused-workflow.md').split('{{')[0]
    background = resource('notifications/session-work-stop-refused-background.md').split('{{')[0]
    texts = texts[-7:]
    assert unfinished in texts[2] and background in texts[4] and 't1 (' in texts[4], texts
    assert 't9' in texts[5] and resource('notifications/session-work-stop-refused.md').split('{{')[0] in texts[5], texts[5]
    assert resource('notifications/session-work-stop-wait-declared.md').split('{{')[0] in texts[6], texts[6]
    assert outcomes(session)[-1][1]['outcome'] == 'waiting', outcomes(session)
    wait_stop = stops(session)[-1]
    assert wait_stop['state'] == {'state': 'effective'} and wait_stop['declaration']['condition'] == 'any(t1)', wait_stop
    items = [tuple(row) for row in store_rows('SELECT kind, number FROM items WHERE session=?', session)]
    assert len(items) == 1 and items[0][1] == 1, items
    result['background'] = True

    # 5. Nonvoluntary ends and Stop.
    other, other_channel = new_placed_session()
    turn(other, 'C-REFUSAL', 2)
    assert outcomes(other)[-1][1] == {'outcome': 'provider_refusal'}, outcomes(other)
    assert corrections(captured[-1]['messages']) == 0
    turn(other, 'C-FAIL')
    assert outcomes(other)[-1][1] == {'outcome': 'failed', 'class': 'provider'}, outcomes(other)
    submit(other, 'C-STOP')
    wait(lambda: (f.ROOT / 'holding-stop').exists(), 'held after the declaration')
    f.counter += 1
    other_channel[0].sendall((json.dumps({'type': 'cancel', 'id': f.counter}) + '\n').encode())
    (f.ROOT / 'release-stop').write_text('release')
    wait(lambda: outcomes(other)[-1][1] == {'outcome': 'interrupted_by_user'}, 'Stop recorded')
    assert stops(other)[-1]['state'] == {'state': 'withdrawn', 'cause': 'revoked'}, stops(other)
    result['nonvoluntary'] = True

    # 6. A child declares its own completion.
    parent, _ = new_placed_session()
    before = {p.name for p in (f.home / 'sessions').glob('*.json')}
    turn(parent, 'C-CHILD', 5)
    child = next(json.loads(p.read_text())['id'] for p in (f.home / 'sessions').glob('*.json')
                 if p.name not in before and json.loads(p.read_text()).get('isolated_child'))
    child_request = next(r for r in captured if 'CHILD-TASK' in json.dumps(r['messages']))
    child_declare = tool_def(child_request, 'declare_stop')
    assert child_declare['parameters']['properties']['action']['enum'] == ['complete'], child_declare
    assert child_declare['description'] == resource('tools/declare-stop-child.md')
    assert outcomes(child)[-1][1] == {'outcome': 'completed', 'declared': True}, outcomes(child)
    result['child'] = True

    # 7. A reload in the middle of a correction.
    reloading, _ = new_placed_session()
    before = len(captured)
    reload_input = submit(reloading, 'C-RELOAD')
    wait(lambda: (f.ROOT / 'holding-reload').exists(), 'held on the first correction')
    reloaded = subprocess.run(prefix + ['server', 'reload', '--force', '--json'], env=f.env, cwd=root,
                              capture_output=True, text=True, timeout=120)
    (f.ROOT / 'reload.txt').write_text(reloaded.stdout + reloaded.stderr)
    (f.ROOT / 'release-reload').write_text('release')
    for channel_ in list(channels):
        close(channel_)
    def reconnect():
        try:
            return connect()
        except OSError:
            return None
    admin = wait(reconnect, 'reconnect after reload')
    wait(lambda: any(o[1] == {'outcome': 'stopped_without_declaration', 'cause': 'missing'} for o in outcomes(reloading)),
         'the continued turn exhausts its counter', 180)
    rows = outcomes(reloading)
    assert [o[1]['outcome'] for o in rows] == ['interrupted_by_reload', 'stopped_without_declaration'], rows
    assert rows[0][0] == rows[1][0], 'the continuation keeps the turn'
    final = captured[-1]['messages']
    assert corrections(final) == 2, corrections(final)
    result['reload_mid_correction'] = {'requests': len(captured) - before}

    # Launch opt-in: the capability, a plain launch and an opted-in launch.
    capability = rpc(admin, 'primary_launch_probe')
    assert capability.get('session_work_opt_in') is True, capability
    placement = session_file(session)['location']['placement']
    def launch(opt_in):
        input_ = {'placement': {'kind': 'existing', 'placement': placement},
                  'cwd': {'kind': 'existing', 'path': str(root)}, 'agent': None, 'model': None, 'selfdev': False}
        if opt_in:
            input_['session_work'] = True
        launched = rpc(admin, 'primary_launch', request={'request': str(uuid.uuid4()),
                                                         'expected_revision': ws('status')['revision'], 'input': input_})
        assert launched['response']['status'] == 'launched', launched
        return launched['response']['record']['session']
    plain = launch(False)
    assert not session_file(plain).get('session_work')
    plain_request = turn(plain, 'PLAIN', 1, outcomes_after=0)
    assert tool_def(plain_request[0], 'declare_stop') is None
    assert tool_def(plain_request[0], 'bash')['parameters']['properties']['wake']['description'] != wake
    opted = launch(True)
    assert session_file(opted)['session_work']['role'] == 'primary'
    assert tool_def(turn(opted, 'PLAIN', 1)[0], 'declare_stop') is not None
    result['launch_opt_in'] = True

    assert not failures, failures
    result['status'] = 'passed'
finally:
    for tag in ('stop', 'reload'):
        (f.ROOT / f'release-{tag}').write_text('release')
    try:
        stop_daemon()
    except Exception:
        result.setdefault('cleanup_error', traceback.format_exc())
    result['provider_requests'] = len(captured)
    (f.ROOT / 'captured.json').write_text(json.dumps(captured, indent=1))
    (f.ROOT / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
