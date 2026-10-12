#!/usr/bin/env python3
"""Arrivals and holding on an owned daemon.

Localhost scripted provider only; private state through
scripts/run_isolated_test.py. With features.session_work on:

  1. Undeliverable (I-03). A sanitized copy of a real execution store
     (JCODE_ARRIVALS_STORE_COPY, itself a copy) is opened: version 21 becomes
     22 with every row kept, deliveries whose recipient Session does not
     exist settle as terminal and visible, and their retry stops.
  2. Busy. Mirza's message to a working session joins the running turn at
     its next safe point; no new turn starts.
  3. Completed, hold, runtime restart. A background task started with wake
     false gets its alias in its receipt; its result after completion is
     held, starts no turn and carries a held diagnostic; it stays held across
     a runtime restart, and arrives first, with its note, when Mirza's next
     message starts a turn.
  4. Waiting and detached. With no client attached, a wait on t1 holds t2
     and a queued message; t1's result ends the stop, and one turn receives
     the held arrivals oldest first with their notes, then the end of the
     stop.
  5. Dispositions over the native protocol. Send-and-keep delivers under
     the stop with its prefix; a queued message stays held until it is sent
     with primary_input_dispose revoke-and-send, which withdraws the stop.
  6. Failure. A declaration survives the provider failure that ends its
     turn; the awaited result then ends the stop and wakes the session.
  7. Stopped. After Mirza's Stop a background result is held; his next
     message wakes the session with the held result first.
  8. Crashed. After an unexpected exit an automatic notice waits and an
     answer passes the recovery gate as human input.

Prose is never asserted: texts are located through the seeded resources.
Run: python3 scripts/run_isolated_test.py python3 scripts/verify_arrivals.py --binary <candidate>
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, re, signal, socket, sqlite3, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc = Path(tempfile.mkdtemp(prefix='ar-', dir=os.environ['JCODE_RUNTIME_DIR']))
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
captured, failures, channels = [], [], []
provider_lock = threading.Lock()
result = {'status': 'failed', 'binary': f.BIN, 'root': str(f.ROOT)}
WORKFLOW_LINE = re.compile(r'Workflow file: (/\S+?/workflow\.md)\. ')
TAG = re.compile(r'TAG:([A-Z0-9-]+)')
UNFINISHED = '- [>] build: Build it\n'
DONE = '- [x] build: Build it\n'
# A read-only copy of a real execution store, made outside the isolated run
# (its private HOME hides the real one). Without it, leg 1 is reported as not
# run.
REAL_STORE = Path(os.environ.get('JCODE_ARRIVALS_STORE_COPY', '/nonexistent'))


def text_of(message):
    content = message.get('content')
    if isinstance(content, list):
        return '\n'.join(str(part.get('text', part)) for part in content)
    return str(content or '')


def resource(path):
    """A seeded managed text, without frontmatter."""
    text = (f.home / 'instructions' / path).read_text()
    if text.startswith('---\n'):
        text = text.split('\n---\n', 1)[1]
    return text.strip()


def lead(name):
    """The fixed opening of a seeded note, before its first placeholder."""
    return resource(f'notifications/{name}.md').split('{{')[0]


def workflow_path(messages):
    found = [m.group(1) for message in messages for m in WORKFLOW_LINE.finditer(text_of(message))]
    return found[-1] if found else None


def call(name, arguments):
    return {'tool_calls': [{'index': 0, 'id': f'call-{uuid.uuid4().hex[:8]}', 'type': 'function',
                            'function': {'name': name, 'arguments': json.dumps({'intent': f'Arrival {name}', **arguments})}}]}


def bash_bg(seconds, wake, label):
    return call('bash', {'command': f'sleep {seconds}', 'run_in_background': True, 'wake': wake,
                         'intent': label})


def anchor(messages):
    """The latest tagged user message: its index and tag."""
    for index in range(len(messages) - 1, -1, -1):
        message = messages[index]
        if message.get('role') == 'user':
            found = TAG.findall(text_of(message))
            if found:
                return index, found[-1]
    return None, ''


def tool_results_after(messages, index):
    return sum(1 for message in messages[index + 1:] if message.get('role') == 'tool')


def after(messages, index, marker):
    """The index of the first message after `index` carrying `marker`."""
    for position in range(index + 1, len(messages)):
        if marker in text_of(messages[position]):
            return position
    return None


def script(request):
    messages = request.get('messages', [])
    tools = {tool['function']['name'] for tool in request.get('tools', [])}
    if not tools:
        return {'content': 'Synthetic summary.'}
    index, tag = anchor(messages)
    if index is None:
        return {'content': 'Synthetic answer'}
    path = workflow_path(messages)
    ended = after(messages, index, lead('session-work-stop-ended'))
    done = tool_results_after(messages, index if ended is None else ended)
    def steps(sequence, final):
        return sequence[done]() if done < len(sequence) else final
    if tag == 'H-BUSY':
        return ('hold', 'busy', {'content': 'Working on it.'})
    if tag == 'H-ASIDE':
        return {'content': 'Noted the aside.'}
    if tag == 'A-HOLD':
        return steps([lambda: call('write', {'file_path': path, 'content': DONE}),
                      lambda: bash_bg(6, False, 'quiet job'),
                      lambda: call('declare_stop', {'action': 'complete', 'summary': 'Done', 'leave_running': ['t1']})],
                     {'content': 'Done for now.'})
    if tag == 'A-NEXT':
        return {'content': 'Answered A.'}
    if tag == 'B-WAIT':
        return steps([lambda: call('write', {'file_path': path, 'content': UNFINISHED}),
                      lambda: bash_bg(10, True, 'long job'),
                      lambda: bash_bg(3, True, 'short job'),
                      lambda: call('declare_stop', {'action': 'wait', 'condition': 't1', 'summary': 'Waiting on t1'})],
                     {'content': 'Waiting on t1.'})
    if tag == 'B-NOTE':
        return steps([lambda: call('write', {'file_path': path, 'content': DONE}),
                      lambda: call('declare_stop', {'action': 'complete', 'summary': 'B done'})],
                     {'content': 'B finished.'})
    if tag == 'C-WAIT':
        return steps([lambda: call('write', {'file_path': path, 'content': UNFINISHED}),
                      lambda: bash_bg(600, True, 'very long job'),
                      lambda: call('declare_stop', {'action': 'wait', 'condition': 't1', 'summary': 'Waiting on C'})],
                     {'content': 'Waiting on C.'})
    if tag == 'C-KEEP':
        return {'content': 'Kept and answered.'}
    if tag == 'C-QUEUED':
        return steps([lambda: call('declare_stop', {'action': 'wait', 'condition': 't1', 'summary': 'Waiting again'})],
                     {'content': 'Waiting again.'})
    if tag == 'D-FAIL':
        if ended is None:
            return steps([lambda: call('write', {'file_path': path, 'content': UNFINISHED}),
                          lambda: bash_bg(3, True, 'failing wait job'),
                          lambda: call('declare_stop', {'action': 'wait', 'condition': 't1', 'summary': 'Waiting on D'})],
                         ('status', 400))
        return steps([lambda: call('write', {'file_path': path, 'content': DONE}),
                      lambda: call('declare_stop', {'action': 'complete', 'summary': 'D done'})],
                     {'content': 'D resumed.'})
    if tag == 'E-STOP':
        return steps([lambda: bash_bg(3, True, 'stopped job')], ('hold', 'stop', {'content': 'late'}))
    if tag == 'E-AFTER':
        return {'content': 'E after.'}
    if tag == 'G-CRASH':
        return ('hold', 'crash', {'content': 'late'})
    if tag in ('G-ANSWER', 'G-NOTICE'):
        return {'content': 'G answered.'}
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
        if isinstance(reply, tuple) and reply[0] == 'hold':
            (f.ROOT / f'holding-{reply[1]}').write_text('holding')
            limit = time.monotonic() + 180
            while not (f.ROOT / f'release-{reply[1]}').exists() and time.monotonic() < limit:
                time.sleep(0.05)
            reply = reply[2]
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
        time.sleep(0.1)
    raise TimeoutError(f'{label}: {last!r}')


def new_placed_session():
    channel = connect()
    reply = rpc(channel, 'subscribe', until={'done', 'error'}, working_dir=str(root), selfdev=False, agent='global:jcode')
    assert reply['type'] == 'done', reply
    session = [e['session_id'] for e in f.events if e.get('type') == 'session'][-1]
    proposal = location({'action': 'propose_placement', 'session': session})
    assert proposal['status'] == 'proposal', proposal
    p = proposal['proposal']
    request = {'request': str(uuid.uuid4()), 'session': session, 'working_dir': p['working_dir'],
               'expected_catalog_revision': p['catalog_revision'], 'placement': p['candidates'][p['default']]['placement']}
    # A typed busy rejection means another control commit holds the
    # session's control lease; the same request is retried.
    def place():
        placed = location({'action': 'place', 'request': request})
        if placed['status'] == 'rejected' and placed['issue']['code'] == 'busy':
            return None
        return placed
    placed = wait(place, 'placement', 60)
    assert placed['status'] == 'state' and placed['record']['state'] == 'complete', placed
    return session, channel


def submit(session, text, delivery='next_turn', origin='human', arrival=None, display_role=None):
    """Submit through the native protocol. Human origin stands in for Mirza."""
    input_id = str(uuid.uuid4())
    body = {'id': input_id, 'session': session, 'delivery': delivery, 'content': text, 'images': []}
    if origin:
        body['origin'] = {'kind': origin}
    if arrival:
        body['arrival'] = arrival
    if display_role:
        body['display_role'] = display_role
    receipt = rpc(admin, 'primary_input', input=body)
    assert receipt['type'] == 'primary_input_receipt', receipt
    return input_id, receipt['receipt']


def inspect(session, input_id):
    event = rpc(admin, 'primary_input_inspect', session=session, input=input_id)
    assert event['type'] == 'primary_input_receipt', event
    return event['receipt']


def committed(session, input_id):
    return inspect(session, input_id)['state'] == 'committed'


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
    return [json.loads(body) for (body,) in
            store_rows('SELECT body FROM turn_outcomes WHERE session=? ORDER BY sequence', session)]


def stops(session):
    return [json.loads(body) for (body,) in store_rows('SELECT body FROM stops WHERE session=? ORDER BY number', session)]


def arrivals(session):
    return [json.loads(body) for (body,) in store_rows('SELECT body FROM arrivals WHERE session=? ORDER BY sequence', session)]


def arrival_of_item(session, alias):
    return next((a for a in arrivals(session) if (a.get('item') or {}).get('alias') == alias), None)


def arrival_of_input(session, input_id):
    return next((a for a in arrivals(session) if a['input'] == input_id), None)


def current_stop(session):
    rows = [s for s in stops(session)
            if s['state']['state'] == 'pending' or (s['state']['state'] == 'effective' and 'condition' in s['declaration'])]
    return rows[-1] if rows else None


def ordered(messages, markers):
    """Every marker appears, in this order, across the request's messages."""
    flat = '\n'.join(text_of(m) for m in messages)
    position = -1
    for marker in markers:
        found = flat.find(marker, position + 1)
        assert found > position, (marker, flat[position:position + 2000])
        position = found
    return True


def requests_with(marker):
    return [r for r in captured if any(marker in text_of(m) for m in r['messages'])]


def first_request_with(marker):
    """The first provider request carrying `marker`. An input is committed
    before the request that carries it is sent, so wait for it."""
    return wait(lambda: (requests_with(marker) or [None])[0], f'request with {marker}', 60)


def tool_text(request, needle):
    return next((text_of(m) for m in request['messages'] if m.get('role') == 'tool' and needle in text_of(m)), '')


def execution_rows(query, *params):
    db = sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro', uri=True)
    try:
        return db.execute(query, params).fetchall()
    finally:
        db.close()


def runtime_status():
    value = subprocess.run(prefix + ['runtime', 'status', '--json'], env=f.env, cwd=root,
                           capture_output=True, text=True, timeout=90)
    assert value.returncode == 0, value.stderr[-2000:]
    return json.loads(value.stdout)


def unresolved_recoveries():
    supervision = runtime_status().get('supervision') or {}
    return [item for item in supervision.get('recoveries', []) if not item.get('resolved')]


def daemon_ready():
    try:
        probe = socket.socket(socket.AF_UNIX); probe.settimeout(2); probe.connect(str(f.sockpath)); probe.close()
        return True
    except OSError:
        return False


def reconnect_admin():
    global admin
    for channel_ in list(channels):
        close(channel_)
    admin = wait(lambda: daemon_ready() and connect(), 'reconnect')


def prepare_copied_store():
    """A sanitized copy of the real execution store, before the daemon starts.

    The real store is only read. In the copy, unfinished runs and native
    process records are settled so no fixture can act on a real process; every
    delivery row stays as it was."""
    target = f.home / 'execution'
    target.mkdir(parents=True, exist_ok=True)
    source = sqlite3.connect(f'file:{REAL_STORE}?mode=ro', uri=True)
    copy = sqlite3.connect(target / 'index.sqlite')
    source.backup(copy)
    source.close()
    copy.execute("UPDATE runs SET state='interrupted' WHERE state IN ('prepared','queued','running')")
    copy.execute('DELETE FROM native_processes')
    copy.execute("UPDATE command_handoffs SET state='finished' WHERE state<>'finished'")
    copy.commit()
    version = copy.execute('PRAGMA user_version').fetchone()[0]
    rows = copy.execute('SELECT d.run_id, r.session_id, r.state, d.notify, d.wake, d.notify_state, d.wake_state '
                        'FROM background_deliveries d JOIN runs r ON r.id=d.run_id').fetchall()
    copy.close()
    terminal = ('completed', 'failed', 'cancelled', 'interrupted')
    pending = [r for r in rows if r[2] in terminal and ((r[3] and r[5] == 'pending') or (r[4] and r[6] == 'pending'))]
    return {'version': version, 'rows': len(rows), 'pending': [r[0] for r in pending],
            'delivered': sorted((r[0], r[5], r[6]) for r in rows)}


def start_daemon(seconds=180):
    """Start the fixture daemon. The first launch of a freshly copied binary
    can take far longer than the shared helper's 30 s (executable scanning,
    machine load), so this waits longer and records how long it took."""
    started = time.monotonic()
    f.proc = subprocess.Popen(f.args, env=f.env, stdout=f.log, stderr=f.log)
    deadline = started + seconds
    while True:
        if f.proc.poll() is not None:
            raise RuntimeError('daemon exited: ' + (f.ROOT / 'server.log').read_text(errors='replace')[-3000:])
        client = socket.socket(socket.AF_UNIX)
        client.settimeout(30)
        try:
            client.connect(str(f.sockpath))
            break
        except (FileNotFoundError, ConnectionRefusedError):
            client.close()
            if time.monotonic() > deadline:
                f.proc.kill()
                raise
            time.sleep(0.1)
    f.client, f.reader = client, client.makefile('rb')
    result.setdefault('startup_seconds', []).append(round(time.monotonic() - started, 1))


def stop_daemon():
    for channel_ in list(channels):
        close(channel_)
    if f.reader is not None:
        f.reader.close(); f.client.close()
        f.reader = f.client = None
    # Never leave a fixture daemon behind: a quiescing daemon gets 60 s, then
    # it is killed, including any process a reload put on the socket.
    listening = subprocess.run(['lsof', '-t', str(f.sockpath)], capture_output=True, text=True).stdout.split()
    others = {int(pid) for pid in listening} - {f.proc.pid if f.proc else None}
    for pid in others:
        os.kill(pid, signal.SIGTERM)
    if f.proc is not None and f.proc.poll() is None:
        f.proc.terminate()
        try:
            f.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            result.setdefault('killed_after_sigterm', []).append(f.proc.pid)
            f.proc.kill(); f.proc.wait(timeout=30)
    for pid in others:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


try:
    copied = prepare_copied_store() if REAL_STORE.exists() else None
    start_daemon(); admin = connect()
    seed = connect()
    assert rpc(seed, 'subscribe', until={'done', 'error'}, working_dir=str(root), selfdev=False)['type'] == 'done'
    close(seed)
    wait(lambda: (f.home / 'instructions' / 'agents').is_dir(), 'seeded instruction store')
    ws('initialize', request=str(uuid.uuid4()))
    review = ws('review', expected_revision=ws('status')['revision'], change={
        'action': 'register_location', 'name': 'work', 'path': str(root), 'registration': {'kind': 'standalone'}})
    ws('apply', request=str(uuid.uuid4()), review=review['id'])
    capability = rpc(admin, 'primary_control_probe')
    assert capability.get('arrival_version') == 1, capability

    # 1. Undeliverable on a copied store.
    if copied is not None:
        assert copied['version'] == 21, copied
        def settled():
            rows = execution_rows('SELECT run_id, notify, wake, notify_state, wake_state, issue FROM background_deliveries')
            by_id = {r[0]: r for r in rows}
            left = [i for i in copied['pending']
                    if any(enabled and state == 'pending' for enabled, state in ((by_id[i][1], by_id[i][3]), (by_id[i][2], by_id[i][4])))]
            return (not left) and by_id
        by_id = wait(settled, 'missing recipients settle', 60)
        assert execution_rows('PRAGMA user_version')[0][0] == 22
        assert len(by_id) >= copied['rows'], 'every row is kept'
        for run_id in copied['pending']:
            row = by_id[run_id]
            assert 'undeliverable' in (row[3], row[4]) and 'does not exist' in (row[5] or ''), row
        for run_id, notify_state, wake_state in copied['delivered']:
            row = by_id[run_id]
            if 'delivered' in (notify_state, wake_state):
                assert (notify_state != 'delivered' or row[3] == 'delivered') and \
                       (wake_state != 'delivered' or row[4] == 'delivered'), (run_id, row)
        log_before = (f.ROOT / 'server.log').read_text(errors='replace').count('Completion recipient location is unavailable')
        time.sleep(12)
        log_after = (f.ROOT / 'server.log').read_text(errors='replace').count('Completion recipient location is unavailable')
        assert log_after == log_before, 'no retry loop after settling'
        result['undeliverable'] = {'rows': copied['rows'], 'settled': len(copied['pending'])}
        # The copied store holds a real runtime's whole history, whose global
        # task listing is slow; the remaining legs run on a fresh one. The
        # copy stays in the artifacts.
        stop_daemon()
        wait(lambda: not daemon_ready(), 'daemon stopped', 60)
        (f.home / 'execution').rename(f.ROOT / 'copied-execution')
        start_daemon(); admin = connect()
    else:
        result['undeliverable'] = 'not run: JCODE_ARRIVALS_STORE_COPY is not set'

    # 2. Busy: an aside joins the running turn.
    busy, busy_channel = new_placed_session()
    submit(busy, 'TAG:H-BUSY')
    wait(lambda: (f.ROOT / 'holding-busy').exists(), 'busy turn holding')
    aside, aside_receipt = submit(busy, 'TAG:H-ASIDE', delivery='safe_boundary')
    assert arrival_of_input(busy, aside)['route'] == {'route': 'deliver'}, arrivals(busy)
    before = len(captured)
    (f.ROOT / 'release-busy').write_text('release')
    wait(lambda: committed(busy, aside), 'aside committed')
    wait(lambda: len(outcomes(busy)) >= 1, 'busy outcome')
    time.sleep(1)
    assert outcomes(busy) == [{'outcome': 'completed', 'declared': False}], outcomes(busy)
    assert len(captured) == before + 1 and ordered(captured[-1]['messages'], ['TAG:H-BUSY', 'TAG:H-ASIDE'])
    result['busy'] = True

    # 3. Completed, hold policy.
    quiet, _ = new_placed_session()
    submit(quiet, 'TAG:A-HOLD')
    wait(lambda: outcomes(quiet), 'A outcome')
    assert outcomes(quiet)[-1] == {'outcome': 'completed', 'declared': True}, outcomes(quiet)
    receipt_text = tool_text(captured[-1], 'Background execution accepted')
    assert '\nAlias: t1' in receipt_text, receipt_text
    held = wait(lambda: arrival_of_item(quiet, 't1'), 'quiet result arrives', 60)
    assert held['route'] == {'route': 'held', 'held_while': {'while': 'stopped'}}, held
    requests_before = len(captured)
    time.sleep(3)
    assert len(captured) == requests_before, 'a held result starts no turn'
    assert inspect(quiet, held['input'])['issue'] == "Held until the session's next turn.", inspect(quiet, held['input'])
    restarted = subprocess.run(prefix + ['server', 'reload', '--force', '--json'], env=f.env, cwd=root,
                               capture_output=True, text=True, timeout=180)
    (f.ROOT / 'reload.txt').write_text(restarted.stdout + restarted.stderr)
    time.sleep(2)
    reconnect_admin()
    time.sleep(3)
    assert inspect(quiet, held['input'])['state'] == 'accepted', 'held across the restart'
    assert len(captured) == requests_before, 'the restart delivered nothing'
    nxt, _ = submit(quiet, 'TAG:A-NEXT')
    wait(lambda: committed(quiet, nxt), 'A-NEXT committed')
    wake_request = first_request_with('TAG:A-NEXT')
    assert ordered(wake_request['messages'], ['TAG:A-HOLD', lead('session-work-held-item-stopped'), 't1 (quiet job)', 'TAG:A-NEXT'])
    assert committed(quiet, held['input'])
    result['completed_hold_restart'] = {'reload_exit': restarted.returncode}
    # A reloaded fixture daemon keeps existing sessions' routes but not the
    # fixture's command-line provider profile for new sessions; start it again
    # with its arguments.
    stop_daemon()
    wait(lambda: not daemon_ready(), 'daemon stopped', 60)
    start_daemon(); admin = connect()

    # 4. Waiting and detached.
    waiting, waiting_channel = new_placed_session()
    submit(waiting, 'TAG:B-WAIT')
    wait(lambda: outcomes(waiting) and outcomes(waiting)[-1]['outcome'] == 'waiting', 'B waiting')
    close(waiting_channel)
    t2 = wait(lambda: arrival_of_item(waiting, 't2'), 't2 arrives', 60)
    assert t2['route'] == {'route': 'held', 'held_while': {'while': 'waiting', 'condition': 't1'}}, t2
    note, note_receipt = submit(waiting, 'TAG:B-NOTE')
    assert note_receipt['state'] == 'accepted' and note_receipt['issue'] == 'Held: the session is waiting on t1.', note_receipt
    assert arrival_of_input(waiting, note)['route']['route'] == 'held'
    t1 = wait(lambda: arrival_of_item(waiting, 't1'), 't1 arrives', 60)
    assert t1['route'] == {'route': 'ends_stop', 'stop': 1}, t1
    wait(lambda: committed(waiting, note), 'B note delivered')
    wake_request = first_request_with('TAG:B-NOTE')
    assert ordered(wake_request['messages'], [
        lead('session-work-held-item-waiting'), 't2 (short job)', lead('session-work-held-message-waiting'),
        'TAG:B-NOTE', lead('session-work-stop-ended'), 't1 '])
    assert stops(waiting)[0]['state'] == {'state': 'withdrawn', 'cause': 'awaited_arrival'}, stops(waiting)
    wait(lambda: outcomes(waiting)[-1] == {'outcome': 'completed', 'declared': True}, 'B completes')
    result['waiting_detached'] = True

    # 5. Dispositions, and held input across a runtime restart.
    disposed, disposed_channel = new_placed_session()
    submit(disposed, 'TAG:C-WAIT')
    wait(lambda: outcomes(disposed) and outcomes(disposed)[-1]['outcome'] == 'waiting', 'C waiting')
    keep, _ = submit(disposed, 'TAG:C-KEEP', arrival={'class': 'human', 'disposition': 'send_keep_stop'})
    wait(lambda: committed(disposed, keep), 'kept message delivered')
    keep_request = first_request_with('TAG:C-KEEP')
    assert ordered(keep_request['messages'], [resource('notifications/session-work-message-kept-stop.md'), 'TAG:C-KEEP'])
    assert current_stop(disposed)['number'] == 1, stops(disposed)
    queued, queued_receipt = submit(disposed, 'TAG:C-QUEUED')
    # A next-turn message sent while the kept-stop turn is still running is
    # decided when that turn ends.
    wait(lambda: inspect(disposed, queued)['issue'] == 'Held: the session is waiting on t1.', 'C-QUEUED held')
    time.sleep(3)
    assert inspect(disposed, queued)['state'] == 'accepted' and not requests_with('TAG:C-QUEUED'), 'held'
    disposed_event = rpc(admin, 'primary_input_dispose', session=disposed, input=queued, disposition='revoke_and_send')
    assert disposed_event['type'] == 'primary_input_receipt', disposed_event
    wait(lambda: committed(disposed, queued), 'revoked message delivered')
    queued_request = first_request_with('TAG:C-QUEUED')
    assert ordered(queued_request['messages'], [resource('notifications/session-work-message-revoked-stop.md'), 'TAG:C-QUEUED'])
    assert stops(disposed)[0]['state'] == {'state': 'withdrawn', 'cause': 'revoked'}, stops(disposed)
    result['dispositions'] = True

    # 6. A declaration survives a failure; the awaited result wakes.
    failing, _ = new_placed_session()
    submit(failing, 'TAG:D-FAIL')
    wait(lambda: outcomes(failing) and outcomes(failing)[-1] == {'outcome': 'failed', 'class': 'provider'}, 'D failed')
    assert stops(failing)[-1]['state'] == {'state': 'effective'}, stops(failing)
    wait(lambda: (arrival_of_item(failing, 't1') or {}).get('route') == {'route': 'ends_stop', 'stop': 1}, 'D t1 ends stop', 60)
    wait(lambda: outcomes(failing)[-1] == {'outcome': 'completed', 'declared': True}, 'D resumes and completes')
    assert any(lead('session-work-stop-ended') in text_of(m) for m in captured[-1]['messages'])
    result['survives_failure'] = True

    # 7. Stopped by Mirza.
    stopped, stopped_channel = new_placed_session()
    submit(stopped, 'TAG:E-STOP')
    wait(lambda: (f.ROOT / 'holding-stop').exists(), 'E holding')
    f.counter += 1
    stopped_channel[0].sendall((json.dumps({'type': 'cancel', 'id': f.counter}) + '\n').encode())
    (f.ROOT / 'release-stop').write_text('release')
    wait(lambda: outcomes(stopped) and outcomes(stopped)[-1] == {'outcome': 'interrupted_by_user'}, 'E stopped')
    held = wait(lambda: arrival_of_item(stopped, 't1'), 'E result arrives', 60)
    assert held['route'] == {'route': 'held', 'held_while': {'while': 'stopped'}}, held
    requests_before = len(captured)
    time.sleep(3)
    assert len(captured) == requests_before
    after_id, _ = submit(stopped, 'TAG:E-AFTER')
    wait(lambda: committed(stopped, after_id), 'E-AFTER delivered')
    assert ordered(first_request_with('TAG:E-AFTER')['messages'],
                   [lead('session-work-held-item-stopped'), 't1 (stopped job)', 'TAG:E-AFTER'])
    result['stopped'] = True

    # 8. Crash: automatic input waits, an answer passes as human input.
    crashed, _ = new_placed_session()
    submit(crashed, 'TAG:G-CRASH')
    wait(lambda: (f.ROOT / 'holding-crash').exists(), 'G holding')
    for channel_ in list(channels):
        close(channel_)
    # Identify the daemon by its listening socket: a reload may have replaced
    # the process the fixture started.
    listening = subprocess.run(['lsof', '-t', str(f.sockpath)], capture_output=True, text=True).stdout.split()
    pids = {int(pid) for pid in listening}
    assert pids, 'no process listens on the fixture socket'
    for pid in pids:
        os.kill(pid, signal.SIGKILL)
    wait(lambda: not daemon_ready(), 'daemon gone', 30)
    if f.proc.poll() is None and f.proc.pid in pids:
        f.proc.wait(timeout=30)
    (f.ROOT / 'release-crash').write_text('release')
    start_daemon(); admin = connect()
    items = wait(lambda: [i for i in unresolved_recoveries() if i['session'] == crashed], 'recovery item', 60)
    notice, _ = submit(crashed, 'TAG:G-NOTICE', delivery='safe_boundary', origin=None)
    requests_before = len(captured)
    time.sleep(4)
    assert inspect(crashed, notice)['state'] == 'accepted' and len(captured) == requests_before, 'automatic input waits'
    answer, _ = submit(crashed, 'TAG:G-ANSWER', origin=None, arrival={'class': 'answer', 'item': {
        'request': 'question:fixture', 'kind': 'question', 'label': 'fixture question', 'policy': 'wake'}})
    wait(lambda: committed(crashed, answer), 'the answer passes the recovery gate')
    assert not [i for i in unresolved_recoveries() if i['session'] == crashed], 'the answer resolved recovery'
    assert ordered(first_request_with('TAG:G-ANSWER')['messages'],
                   [lead('session-work-held-notice-stopped'), 'TAG:G-NOTICE', 'TAG:G-ANSWER'])
    result['crash'] = {'recovery_items': len(items)}

    assert not failures, failures
    result['status'] = 'passed'
finally:
    for tag in ('busy', 'stop', 'crash'):
        (f.ROOT / f'release-{tag}').write_text('release')
    try:
        stop_daemon()
    except Exception:
        result.setdefault('cleanup_error', traceback.format_exc())
    result['provider_requests'] = len(captured)
    (f.ROOT / 'captured.json').write_text(json.dumps(captured, indent=1))
    (f.ROOT / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
