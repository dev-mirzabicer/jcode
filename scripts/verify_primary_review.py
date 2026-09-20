#!/usr/bin/env python3
"""Native regressions for independent review F1/F2. Owned fixtures and local models only."""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, socket, tempfile, time, threading, uuid
from pathlib import Path
import test_instruction_manager as f
ipc = Path(tempfile.mkdtemp(prefix='pr-', dir=os.environ['JCODE_RUNTIME_DIR']))
old = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if a == old else a for a in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
cfg = f.home / 'config.toml'
cfg.write_text(cfg.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))
channels = []
events = {}
counter = 100
called = threading.Event()

def models(self):
    payload = json.dumps({'object': 'list', 'data': [{'id': name, 'object': 'model', 'owned_by': 'fixture'} for name in ['fixture', 'fixture-other']]}).encode()
    self.send_response(200)
    self.send_header('Content-Type', 'application/json')
    self.send_header('Content-Length', str(len(payload)))
    self.end_headers()
    self.wfile.write(payload)

def complete(self):
    f.posts.append(self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode())
    called.set()
    chunks = [{'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {'content': 'Synthetic review repair completed'}, 'finish_reason': None}]}, {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}]}]
    payload = (''.join(('data: ' + json.dumps(c) + '\n\n' for c in chunks)) + 'data: [DONE]\n\n').encode()
    self.send_response(200)
    self.send_header('Content-Type', 'text/event-stream')
    self.send_header('Content-Length', str(len(payload)))
    self.end_headers()
    self.wfile.write(payload)
f.FixtureProvider.do_GET = models
f.FixtureProvider.do_POST = complete

def conn():
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(120)
    s.connect(str(f.sockpath))
    r = s.makefile('rb')
    channels.append((s, r))
    events[id(s)] = []
    return (s, r)

def req(ch, kind, **fields):
    global counter
    counter += 1
    n = counter
    s, r = ch
    s.sendall((json.dumps({'type': kind, 'id': n, **fields}) + '\n').encode())
    while True:
        line = r.readline()
        assert line, 'unexpected peer disconnect'
        event = json.loads(line)
        events[id(s)].append(event)
        if event.get('id') == n and event.get('type') != 'ack' and (kind not in ('subscribe', 'clear', 'set_model') or event.get('type') in ('done', 'error', 'startup_context_failed', 'model_changed')):
            return event

def attach(ch, session):
    response = req(ch, 'subscribe', working_dir=str(f.project), target_session_id=session, selfdev=False, client_instance_id=str(id(ch)), client_has_local_history=False, allow_session_takeover=False)
    assert response['type'] == 'done', response

def idle(ch, session):
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        state = req(ch, 'state')
        assert state['session_id'] == session, state
        if not state['is_processing']:
            return
        time.sleep(0.05)
    raise TimeoutError('primary did not settle')

def ws(action, **fields):
    event = req(admin, 'workspace', request={'action': action, **fields})
    assert event['type'] == 'workspace_response', event
    return event['response']['value']

def load(session):
    return json.loads((f.home / 'sessions' / (session + '.json')).read_text())

def close_channels():
    for s, r in channels:
        r.close()
        s.close()
    channels.clear()
try:
    f.start()
    admin = conn()
    ws('initialize', request=str(uuid.uuid4()))
    launch = {'request': str(uuid.uuid4()), 'expected_revision': ws('status')['revision'], 'input': {'placement': {'kind': 'standalone', 'root': str(f.project)}, 'cwd': {'kind': 'existing', 'path': str(f.project)}, 'agent': None, 'model': None, 'selfdev': False}}
    response = req(admin, 'primary_launch', request=launch)
    assert response['response']['status'] == 'launched', response
    source = response['response']['record']['session']
    assert not f.posts
    reply = req(admin, 'notify_session', session_id=source, message='DETACHED_REVIEW_NOTIFICATION')
    assert reply['type'] == 'done' and called.wait(30), reply
    a = conn()
    attach(a, source)
    idle(a, source)
    first = req(a, 'get_history')
    assert sum(('DETACHED_REVIEW_NOTIFICATION' in m['content'] for m in first['messages'])) == 1
    a[1].close()
    a[0].close()
    channels.remove(a)
    called.clear()
    reply = req(admin, 'notify_session', session_id=source, message='FORMER_OBSERVER_REVIEW_NOTIFICATION')
    assert reply['type'] == 'done' and called.wait(30), reply
    a = conn()
    b = conn()
    attach(a, source)
    attach(b, source)
    idle(a, source)
    assert len(f.posts) == 2, f.posts
    missing = req(admin, 'notify_session', session_id='missing-review-target', message='must fail')
    assert missing['type'] == 'error', missing
    before = load(source)
    assert before.get('location')
    response = req(a, 'clear')
    assert response['type'] == 'done', response
    new = [e['session_id'] for e in events[id(a[0])] if e['type'] == 'session'][-1]
    replacement = load(new)
    assert new != source and replacement['location']['placement'] == before['location']['placement']
    assert replacement['location']['cwd'] == before['location']['cwd']
    assert replacement['primary_creation']['request'] != before['primary_creation']['request']
    assert replacement['system_prompt']['active_agent'] == before['system_prompt']['active_agent']
    assert replacement['startup_context']['prepared_at'] != before['startup_context']['prepared_at']
    assert replacement['model'] == before['model'] and replacement.get('reasoning_effort') == before.get('reasoning_effort')
    assert req(b, 'state')['session_id'] == source
    assert req(a, 'state')['session_id'] == new
    assert req(a, 'set_model', model='fixture-other')['type'] == 'model_changed'
    assert req(b, 'get_history')['provider_model'] == 'fixture'
    assert req(a, 'get_history')['provider_model'] == 'fixture-other'
    rows = ws('sessions', target=None, after=None, limit=200)
    assert any((row['session'] == new and row['placement'] == replacement['location']['placement'] and row['reconciled'] for row in rows)), rows
    legacy_a = conn()
    assert req(legacy_a, 'subscribe', working_dir=str(f.project), selfdev=False)['type'] == 'done'
    legacy = [e['session_id'] for e in events[id(legacy_a[0])] if e['type'] == 'session'][-1]
    legacy_b = conn()
    attach(legacy_b, legacy)
    assert req(legacy_a, 'clear')['type'] == 'done'
    assert req(legacy_b, 'state')['session_id'] == legacy
    close_channels()
    f.reader.close()
    f.client.close()
    f.proc.terminate()
    f.proc.wait(timeout=30)
    f.start()
    admin = conn()
    restored = conn()
    attach(restored, new)
    assert req(restored, 'get_history')['provider_model'] == 'fixture-other'
    assert load(new)['location'] == replacement['location']
    assert load(new)['system_prompt']['active_agent'] == replacement['system_prompt']['active_agent']
    assert any((row['session'] == new and row['reconciled'] for row in ws('sessions', target=None, after=None, limit=200)))
    assert len(f.posts) == 2
    result = {'root': str(f.ROOT), 'binary': f.BIN, 'detached_notify': True, 'post_detach_notify': True, 'unknown_rejected': True, 'legacy_and_managed_peers_survive': True, 'managed_location_and_index': True, 'model_isolation': True, 'restart_restore': True, 'provider_calls': len(f.posts), 'source': source, 'replacement': new}
    (f.ROOT / 'review-repair-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
    close_channels()
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:
            f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            f.proc.kill()
            f.proc.wait(timeout=5)
    f.http.shutdown()
    f.http.server_close()
    f.log.close()
    (f.ROOT / 'review-repair-cleanup.json').write_text(json.dumps({'daemon_exit': f.proc.returncode if f.proc else None, 'provider_calls': len(f.posts)}))
    (f.ROOT / 'review-repair-events.json').write_text(json.dumps(events, indent=2))
