#!/usr/bin/env python3
"""Session-work foundation acceptance on an owned daemon.

Localhost scripted provider only; private state through
scripts/run_isolated_test.py. With features.session_work on, a TUI-created
session placed in a registered root gets a session-work binding and its
Session Context names its workflow file. The scripted model then:

  1. writes a valid workflow.md (revision 1, file and history written);
  2. writes an invalid one (refused with a line number, nothing changes);
  3. edits it with a shell command (restored before the next provider
     request, with one notice);
  4. edits it with the edit tool (revision 2);
  5. delegates to a child with a fixture task preset whose workflow template
     becomes the child's revision 1; the child edits its own workflow file.

The daemon restarts: the binding and store survive, and a shell edit made
while it was down is restored with a notice at the next request. A split
starts without a workflow, a transfer copies it with provenance. With the
flag off after another restart, a new session has no session work and cannot
write a session-work path, while the activated session keeps its rules.

Run: python3 scripts/run_isolated_test.py python3 scripts/verify_session_work.py --binary <candidate>
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, re, socket, sqlite3, stat, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc = Path(tempfile.mkdtemp(prefix='sw-', dir=os.environ['JCODE_RUNTIME_DIR']))
old_socket = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if arg == old_socket else arg for arg in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
f.env.pop('JCODE_SESSION_WORK_ENABLED', None)
config = f.home / 'config.toml'
BASE_CONFIG = config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1)
root = (f.ROOT / 'work').resolve()
root.mkdir()
captured, failures, channels = [], [], []
provider_lock = threading.Lock()
result = {'status': 'failed', 'binary': f.BIN, 'root': str(f.ROOT)}
WORKFLOW_LINE = re.compile(r'Workflow file: (/\S+?/workflow\.md)\. ')
RESTORED = 'was changed outside the file tools and has been restored to revision'
VALID = '- [>] ground: Understand the fixture {research}\n- [ ] build: Build it\n'
EDITED = '- [x] ground: Understand the fixture {research}\n- [ ] build: Build it\n'
INVALID = '- [>] ground: One\n- [>] build: Two\n'
TEMPLATE = '- [>] gather: Gather sources\n- [ ] report: Report findings\n'


def set_flag(enabled):
    config.write_text(BASE_CONFIG.replace('[features]', f'[features]\nsession_work={str(enabled).lower()}', 1))


def text_of(message):
    content = message.get('content')
    if isinstance(content, list):
        return '\n'.join(str(part.get('text', part)) for part in content)
    return str(content or '')


def workflow_path(messages):
    """The latest workflow file the transcript names (a split names its own last)."""
    found = [m.group(1) for message in messages for m in WORKFLOW_LINE.finditer(text_of(message))]
    return found[-1] if found else None


def tool_call(name, arguments):
    return {'tool_calls': [{'index': 0, 'id': f'call-{uuid.uuid4().hex[:8]}', 'type': 'function',
                            'function': {'name': name, 'arguments': json.dumps({'intent': 'Session-work fixture', **arguments})}}]}


def tool_results_since_user(messages):
    count = 0
    for message in reversed(messages):
        if message.get('role') == 'user' and not text_of(message).lstrip().startswith('<system-reminder>'):
            break
        if message.get('role') == 'tool':
            count += 1
    return count


def script(request):
    messages = request.get('messages', [])
    tools = {tool['function']['name'] for tool in request.get('tools', [])}
    users = [text_of(m) for m in messages if m.get('role') == 'user'
             and not text_of(m).lstrip().startswith('<system-reminder>')]
    latest = users[-1] if users else ''
    done = tool_results_since_user(messages)
    path = workflow_path(messages)
    if not tools:
        # A tool-less request is the transfer handoff summary.
        return {'content': 'Synthetic handoff summary of the fixture session.'}
    if 'subagent' not in tools and 'CHILD-TASK' in latest:
        # The isolated child completes its template's first module.
        if done == 0:
            return tool_call('edit', {'file_path': path, 'old_string': '- [>] gather', 'new_string': '- [x] gather'})
        if done == 1:
            return tool_call('edit', {'file_path': path, 'old_string': '- [ ] report', 'new_string': '- [>] report'})
        return {'content': 'CHILD DONE'}
    if done:
        return {'content': 'Fixture step done'}
    if latest.endswith('W-VALID'):
        return tool_call('write', {'file_path': path, 'content': VALID})
    if latest.endswith('W-INVALID'):
        return tool_call('write', {'file_path': path, 'content': INVALID})
    if latest.endswith('W-SHELL'):
        return tool_call('bash', {'command': f"printf 'tampered by a shell\\n' > '{path}'"})
    if latest.endswith('W-EDIT'):
        return tool_call('edit', {'file_path': path, 'old_string': '- [>] ground', 'new_string': '- [x] ground'})
    if latest.endswith('W-CHILD'):
        return tool_call('subagent', {'agent': 'fixture', 'model_alias': 'fixture', 'permission': 'read_only',
                                      'preset': 'fixture', 'prompt': 'CHILD-TASK', 'disable_startup_context': True})
    if latest.endswith('W-OFFWRITE'):
        target = str(f.home / 'session-work' / OFF_SESSION / 'workflow.md')
        return tool_call('write', {'file_path': target, 'content': VALID})
    return {'content': 'Synthetic answer'}


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
OFF_SESSION = None


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
    """A session the attached TUI creates (Subscribe), placed in the fixture root."""
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


def turn(session, text, expected_requests):
    before = len(captured)
    input_id = str(uuid.uuid4())
    receipt = rpc(admin, 'primary_input', input={'id': input_id, 'session': session,
                                                 'delivery': 'safe_boundary', 'content': text, 'images': []})
    assert receipt['type'] == 'primary_input_receipt', receipt
    wait(lambda: len(captured) >= before + expected_requests, f'{text}: provider requests')
    def settled():
        state = rpc(admin, 'primary_input_inspect', session=session, input=input_id)
        return state['receipt']['state'] in ('committed', 'failed')
    wait(settled, f'{text}: input settled')
    time.sleep(1.0)
    assert len(captured) == before + expected_requests, (text, len(captured) - before)
    return captured[before:]


def session_file(session):
    return json.loads((f.home / 'sessions' / f'{session}.json').read_text())


def store_path():
    """The daemon's durable state (JCODE_RUNTIME_DIR/durable-state when set)."""
    for candidate in (Path(f.env['JCODE_RUNTIME_DIR']) / 'durable-state' / 'session-work' / 'store.sqlite3',
                      f.home / 'state' / 'session-work' / 'store.sqlite3'):
        if candidate.exists():
            return candidate
    return None


def store_rows(query, *params):
    db = sqlite3.connect(f'file:{store_path()}?mode=ro', uri=True)
    try:
        return db.execute(query, params).fetchall()
    finally:
        db.close()


def head(session):
    rows = store_rows('SELECT revision, text FROM workflow_revisions WHERE session=? ORDER BY revision DESC LIMIT 1', session)
    return tuple(rows[0]) if rows else None


def tool_texts(request):
    return [text_of(m) for m in request['messages'] if m.get('role') == 'tool']


def mode(path):
    return stat.S_IMODE(path.stat().st_mode)


def stop_daemon():
    for channel in list(channels):
        close(channel)
    f.reader.close(); f.client.close(); f.proc.terminate(); f.proc.wait(timeout=60)


try:
    set_flag(True)
    f.start(); admin = connect()
    # The first session seeds the global instruction store.
    seed = connect()
    assert rpc(seed, 'subscribe', until={'done', 'error'}, working_dir=str(root), selfdev=False)['type'] == 'done'
    close(seed)
    wait(lambda: (f.home / 'instructions' / 'agents').is_dir(), 'seeded instruction store')
    # Fixture instructions: an isolated-capable agent, a roster alias, a task
    # preset carrying a workflow template, and a module type.
    instructions = f.home / 'instructions'
    (instructions / 'agents/fixture.md').write_text('---\nid: fixture\nkind: agent\nname: Fixture\ndescription: Synthetic native fixture\navailability: both\n---\nSYNTHETIC PROFILE')
    (instructions / 'model-roster.toml').write_text('[aliases.fixture]\ndescription="Synthetic local alias"\nmodels=["wp09-fixture:fixture"]\n')
    (instructions / 'notifications/task-preset.fixture.md').write_text(
        '---\nid: task-preset.fixture\nkind: notification\nname: Fixture\ndescription: Synthetic preset\nworkflow: |\n'
        + ''.join('  ' + line + '\n' for line in TEMPLATE.splitlines()) + '---\nSYNTHETIC PRESET')
    (instructions / 'module-types').mkdir(exist_ok=True)
    (instructions / 'module-types/research.md').write_text(
        '---\nid: research\nkind: module-type\nname: Research\ndescription: Synthetic module type\nsubtypes:\n  - web\n---\nSynthetic body.\n')
    ws('initialize', request=str(uuid.uuid4()))
    review = ws('review', expected_revision=ws('status')['revision'], change={
        'action': 'register_location', 'name': 'work', 'path': str(root), 'registration': {'kind': 'standalone'}})
    ws('apply', request=str(uuid.uuid4()), review=review['id'])

    # Activation of a TUI-created hosted session.
    session, _ = new_placed_session()
    binding = session_file(session).get('session_work')
    assert binding and binding['role'] == 'primary', binding
    expected_path = str(f.home / 'session-work' / session / 'workflow.md')
    assert expected_path in binding['context_line'], binding
    store = store_path()
    assert store and mode(store) == 0o600, store
    assert mode(store.parent) == 0o700
    assert store_rows('PRAGMA journal_mode')[0][0] == 'wal'
    frozen = json.loads(store_rows('SELECT body FROM activation WHERE session=?', session)[0][0])
    assert [t['id'] for t in frozen['module_types']] == ['global:research'], frozen
    workdir = Path(expected_path).parent
    assert workdir.is_dir() and mode(workdir) == 0o700
    result['activation'] = True

    # Other creators stay without session work while the flag is on: a Harness
    # primary_launch and a process-owned `jcode run`.
    launched = rpc(admin, 'primary_launch', request={'request': str(uuid.uuid4()), 'expected_revision': ws('status')['revision'],
        'input': {'placement': {'kind': 'existing', 'placement': session_file(session)['location']['placement']},
                  'cwd': {'kind': 'existing', 'path': str(root)}, 'agent': None, 'model': None, 'selfdev': False}})
    assert launched['response']['status'] == 'launched', launched
    assert not session_file(launched['response']['record']['session']).get('session_work')
    before_run = {p.name for p in (f.home / 'sessions').glob('*.json')}
    ran = subprocess.run([f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture',
                          'run', '--place', 'RUN-PROMPT'], env=f.env, cwd=root, capture_output=True, text=True,
                         timeout=120, stdin=subprocess.DEVNULL)
    (f.ROOT / 'run.txt').write_text(ran.stdout + ran.stderr)
    assert ran.returncode == 0, ran.stderr
    created = [json.loads(p.read_text()) for p in (f.home / 'sessions').glob('*.json') if p.name not in before_run]
    assert created and not any(body.get('session_work') for body in created), created
    result['other_creators_unaffected'] = True

    # 1. A valid write: one request names the path in Session Context.
    first = turn(session, 'W-VALID', 2)
    assert workflow_path(first[0]['messages']) == expected_path, 'Session Context names the workflow file'
    assert head(session) == (1, VALID)
    assert Path(expected_path).read_text() == VALID
    history = workdir / 'history' / 'r01.md'
    assert history.read_text() == VALID and mode(history) == 0o400
    # 2. An invalid write changes nothing.
    second = turn(session, 'W-INVALID', 2)
    refusal = tool_texts(second[-1])[-1]
    assert 'line 2' in refusal, refusal
    assert head(session) == (1, VALID) and Path(expected_path).read_text() == VALID
    # 3. A shell edit is restored before the next request, with one notice.
    third = turn(session, 'W-SHELL', 2)
    after_shell = json.dumps(third[1]['messages'])
    assert RESTORED in after_shell and 'revision 1' in after_shell, 'notice before the next request'
    assert Path(expected_path).read_text() == VALID
    assert head(session) == (1, VALID)
    # 4. The edit tool reads the store's text and commits revision 2.
    turn(session, 'W-EDIT', 2)
    assert head(session) == (2, EDITED), head(session)
    assert (workdir / 'history' / 'r02.md').read_text() == EDITED
    notices = sum(1 for m in captured[-1]['messages'] if RESTORED in text_of(m))
    assert notices == 1, notices
    result['workflow_file'] = True

    # 5. A child from a preset with a workflow template.
    children_before = {p.name for p in (f.home / 'sessions').glob('*.json')}
    turn(session, 'W-CHILD', 5)
    child = wait(lambda: next((json.loads(p.read_text())['id'] for p in (f.home / 'sessions').glob('*.json')
                               if p.name not in children_before and json.loads(p.read_text()).get('isolated_child')), None),
                 'child session')
    child_binding = session_file(child).get('session_work')
    assert child_binding and child_binding['role'] == 'child', child_binding
    revisions = store_rows('SELECT revision, text, source FROM workflow_revisions WHERE session=? ORDER BY revision', child)
    assert revisions[0][1] == TEMPLATE and json.loads(revisions[0][2])['kind'] == 'template', revisions
    assert revisions[-1][1] == TEMPLATE.replace('[>] gather', '[x] gather').replace('[ ] report', '[>] report'), revisions
    assert len(revisions) == 3, revisions
    result['child_template'] = True

    # Restart: binding and store survive; an offline shell edit is restored.
    stop_daemon()
    Path(expected_path).write_text('edited while the daemon was down\n')
    f.start(); admin = connect()
    after = turn(session, 'AFTER-RESTART', 1)
    assert RESTORED in json.dumps(after[0]['messages'][-3:]), 'restore notice after restart'
    assert Path(expected_path).read_text() == EDITED and head(session) == (2, EDITED)
    assert session_file(session)['session_work'] == binding
    result['restart'] = True

    # Split: activated, no workflow, its own file named in the fork notice.
    attached = connect()
    assert rpc(attached, 'subscribe', until={'done', 'error'}, working_dir=str(root), target_session_id=session, selfdev=False)['type'] == 'done'
    split = rpc(attached, 'split', until={'split_response', 'error'})
    assert split['type'] == 'split_response', split
    split_id = split['new_session_id']
    split_binding = session_file(split_id)['session_work']
    assert split_binding['role'] == 'primary' and split_id in split_binding['context_line']
    assert head(split_id) is None
    # The copied transcript names the source's file; the fork notice names its own, last.
    split_messages = [{'content': json.dumps(m)} for m in session_file(split_id)['messages']]
    assert workflow_path(split_messages) == str(f.home / 'session-work' / split_id / 'workflow.md')
    result['split'] = True

    # Transfer: the workflow is copied with provenance.
    sessions_before = {p.name for p in (f.home / 'sessions').glob('*.json')}
    transferred = rpc(attached, 'transfer')
    assert transferred['type'] != 'error', transferred
    def transfer_child():
        for p in (f.home / 'sessions').glob('*.json'):
            if p.name in sessions_before:
                continue
            body = json.loads(p.read_text())
            if body.get('parent_id') == session and not body.get('isolated_child'):
                return body['id']
    transfer_id = wait(transfer_child, 'transfer session')
    copied = store_rows('SELECT revision, text, source FROM workflow_revisions WHERE session=?', transfer_id)
    assert len(copied) == 1 and copied[0][1] == EDITED, copied
    source = json.loads(copied[0][2])
    assert source == {'kind': 'transfer', 'source_session': session, 'source_revision': 2}, source
    result['transfer'] = True

    # Flag off: a new session has no session work; the activated one keeps it.
    stop_daemon()
    set_flag(False)
    f.start(); admin = connect()
    OFF_SESSION, _ = new_placed_session()
    assert not session_file(OFF_SESSION).get('session_work')
    off = turn(OFF_SESSION, 'W-OFFWRITE', 2)
    assert 'Workflow file:' not in json.dumps(off[0]['messages'])
    assert not (f.home / 'session-work' / OFF_SESSION).exists()
    assert tool_texts(off[-1])[-1].startswith('[Error]'), tool_texts(off[-1])
    Path(expected_path).write_text('another shell edit\n')
    kept = turn(session, 'FLAG-OFF', 1)
    assert RESTORED in json.dumps(kept[0]['messages'][-3:])
    assert Path(expected_path).read_text() == EDITED
    result['flag_off'] = True

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
