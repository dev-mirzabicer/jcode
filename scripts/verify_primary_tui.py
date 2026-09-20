#!/usr/bin/env python3
"""Actual native TUI launch, busy navigation and snapshot recovery in owned fixtures."""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, socket, tempfile, threading, time, shlex
from pathlib import Path
import test_instruction_manager as f
ipc = Path(tempfile.mkdtemp(prefix='pt-', dir=os.environ['JCODE_RUNTIME_DIR']))
old = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if a == old else a for a in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
config = f.home / 'config.toml'
config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))
entered = threading.Event()
release = threading.Event()
testers = []
frames = []

def response(self):
    f.posts.append(self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode())
    assert 'SYNTHETIC UI WAIT' in f.posts[-1]
    self.send_response(200)
    self.send_header('Content-Type', 'text/event-stream')
    self.end_headers()

    def chunk(content, stop=None):
        value = {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {'content': content}, 'finish_reason': stop}]}
        self.wfile.write(('data: ' + json.dumps(value) + '\n\n').encode())
        self.wfile.flush()
    chunk('UI fixture prefix ')
    entered.set()
    assert release.wait(180), 'owned UI gate was not released'
    chunk('UI fixture suffix', 'stop')
    self.wfile.write(b'data: [DONE]\n\n')
    self.wfile.flush()
f.FixtureProvider.do_POST = response

def uid():
    import uuid
    return str(uuid.uuid4())

def rpc(kind, **fields):
    f.counter += 1
    identity = f.counter
    f.send({'type': kind, 'id': identity, **fields})
    return f.until(lambda e: e.get('id') == identity and e.get('type') != 'ack')

def workspace(action, **fields):
    event = rpc('workspace', request={'action': action, **fields})
    assert event['type'] == 'workspace_response', event
    return event['response']['value']

def spec(path):
    return {'request': uid(), 'expected_revision': workspace('status')['revision'], 'input': {'placement': {'kind': 'standalone', 'root': str(path)}, 'cwd': {'kind': 'existing', 'path': str(path)}, 'agent': None, 'model': None, 'selfdev': False}}

def command(tid, text):
    entry = next((row for row in json.loads((f.home / 'testers.json').read_text()) if row['id'] == tid))
    target = Path(entry['debug_cmd_path'])
    reply = Path(entry['debug_response_path'])
    if reply.exists():
        reply.unlink()
    temporary = target.with_suffix('.next')
    temporary.write_text(text)
    os.replace(temporary, target)
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if reply.exists():
            value = reply.read_text()
            if value:
                reply.unlink()
                with (f.ROOT / 'tui-commands.jsonl').open('a') as out:
                    out.write(json.dumps({'tester': tid, 'command': text, 'response': value}) + '\n')
                return value
        time.sleep(0.025)
    raise TimeoutError(text)

def wait_state(tid, predicate, operation='state'):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        raw = command(tid, operation)
        try:
            value = json.loads(raw)
        except ValueError:
            value = {}
        if predicate(value):
            return value
        time.sleep(0.05)
    raise TimeoutError(operation + ': ' + raw)

def frame(tid, label):
    deadline = time.monotonic() + 15
    while True:
        raw = command(tid, 'screen-json')
        if raw.lstrip().startswith('{'):
            break
        assert time.monotonic() < deadline, raw
        time.sleep(0.1)
    (f.ROOT / (label + '.json')).write_text(raw)
    frames.append(label)
    return raw

def navigate(tid, session):
    command(tid, 'set_input:/resume')
    command(tid, 'keys:enter')
    wait_state(tid, lambda state: state.get('visible') and session in state.get('sessions', []), 'session-picker-state')
    query = session.split('_')[1]
    command(tid, 'keys:/,' + ','.join(query) + ',enter')
    return wait_state(tid, lambda s: s.get('session_id') == session, 'startup-context-state')
try:
    f.start()
    f.client.settimeout(120)
    workspace('initialize', request=uid())
    other = f.ROOT / 'other'
    other.mkdir()
    second = rpc('primary_launch', request=spec(other))
    assert second['response']['status'] == 'launched', second
    target = second['response']['record']['session']
    f.counter += 1
    attach = f.counter
    f.send({'type': 'subscribe', 'id': attach, 'target_session_id': target, 'working_dir': str(other), 'selfdev': False})
    f.until(lambda event: event.get('type') == 'done' and event.get('id') == attach)
    context = rpc('message', content='Synthetic target context for native navigation', images=[], no_reply=True)
    assert context['type'] == 'context_message_added', context
    launch = spec(f.project)
    document = f.ROOT / 'tui-launch.json'
    document.write_text(json.dumps(launch))
    wrapper = f.ROOT / 'tester.sh'
    wrapper.write_text('#!/bin/sh\nexec ' + ' '.join((shlex.quote(v) for v in [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', '--socket', str(f.sockpath), '--primary-launch', str(document)])) + ' "$@"\n')
    wrapper.chmod(448)
    f.debug('tester:spawn ' + json.dumps({'cwd': str(f.project), 'binary': str(wrapper), 'cols': 120, 'rows': 32}))
    tid = json.loads((f.home / 'testers.json').read_text())[-1]['id']
    testers.append(tid)
    first = wait_state(tid, lambda s: bool(s.get('session_id')), 'startup-context-state')['session_id']
    assert first != target and (not f.posts)
    command(tid, 'keys:esc,esc')
    frame(tid, 'capture-enabled')
    command(tid, 'set_input:synthetic unsent draft')
    frame(tid, 'launch-wide')
    command(tid, 'set_input:')
    command(tid, 'set_input:SYNTHETIC UI WAIT')
    command(tid, 'keys:enter')
    assert entered.wait(30), 'TUI input never reached the provider'
    wait_state(tid, lambda s: s.get('processing') is True)
    frame(tid, 'processing-wide')
    navigate(tid, target)
    assert not release.is_set() and len(f.posts) == 1
    frame(tid, 'navigated-while-source-busy')
    source = json.loads((f.home / 'sessions' / (first + '.json')).read_text())
    assert source['working_dir'] == str(f.project.resolve())
    release.set()
    navigate(tid, first)
    wait_state(tid, lambda s: s.get('processing') is False)
    final = frame(tid, 'completed-return')
    assert 'UI fixture prefix' in final and 'UI fixture suffix' in final, final
    command(tid, 'set_input:/clear')
    command(tid, 'keys:enter')
    cleared = wait_state(tid, lambda state: state.get('session_id') and state['session_id'] != first, 'startup-context-state')['session_id']
    assert json.loads((f.home / 'sessions' / (cleared + '.json')).read_text())['location']['placement'] == source['location']['placement']
    entered.clear()
    command(tid, 'set_input:SYNTHETIC UI WAIT')
    command(tid, 'keys:enter')
    assert entered.wait(30), 'new Clear primary did not accept a turn'
    wait_state(tid, lambda state: state.get('processing') is False)
    clear_output = frame(tid, 'new-context-output')
    assert 'UI fixture prefix' in clear_output and 'UI fixture suffix' in clear_output
    navigate(tid, first)
    wait_state(tid, lambda state: state.get('processing') is False)
    restored_source = frame(tid, 'original-after-clear')
    assert 'UI fixture prefix' in restored_source and 'UI fixture suffix' in restored_source
    f.debug(f'tester:{tid}:stop')
    testers.remove(tid)
    wrapper.write_text('#!/bin/sh\nexec ' + ' '.join((shlex.quote(v) for v in [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', '--socket', str(f.sockpath), '--resume', first])) + ' "$@"\n')
    f.debug('tester:spawn ' + json.dumps({'cwd': str(other), 'binary': str(wrapper), 'cols': 80, 'rows': 24}))
    tid = json.loads((f.home / 'testers.json').read_text())[-1]['id']
    testers.append(tid)
    wait_state(tid, lambda s: s.get('session_id') == first, 'startup-context-state')
    narrow = frame(tid, 'reconnected-narrow')
    assert 'UI fixture prefix' in narrow and 'UI fixture suffix' in narrow, narrow
    assert json.loads((f.home / 'sessions' / (first + '.json')).read_text())['working_dir'] == str(f.project.resolve())
    assert len(f.posts) == 2
    result = {'binary': f.BIN, 'root': str(f.ROOT), 'session': first, 'target': target, 'physical_input': True, 'busy_navigation': True, 'independent_clear': True, 'cwd_preserved': True, 'reconnect': True, 'frames': frames, 'provider_calls': len(f.posts)}
    (f.ROOT / 'primary-tui-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
    release.set()
    cleanup = {}
    for tid in list(testers):
        try:
            cleanup[tid] = f.debug(f'tester:{tid}:stop')
        except Exception as error:
            cleanup[tid] = str(error)
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:
            f.proc.wait(timeout=30)
        except Exception:
            f.proc.kill()
            f.proc.wait(timeout=10)
    cleanup['daemon_exit'] = f.proc.returncode if f.proc else None
    f.http.shutdown()
    f.http.server_close()
    f.log.close()
    (f.ROOT / 'primary-tui-cleanup.json').write_text(json.dumps(cleanup, indent=2))
    (f.ROOT / 'provider-requests.json').write_text(json.dumps(f.posts, indent=2))
