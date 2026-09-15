#!/usr/bin/env python3
"""Private actual-binary delegation acceptance with localhost scripted inference."""
import argparse, http.server, json, os, re, socket, sqlite3, subprocess, tempfile, threading, time, signal, shlex
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--binary', required=True)
p.add_argument('--evidence-parent', required=True)
p.add_argument('--sdk-entry', default=str(Path(__file__).resolve().parents[1] / 'sdk/typescript/dist/index.js'))
o = p.parse_args()
assert os.environ.get('JCODE_TEST_STATE_ROOT'), 'Run through scripts/run_isolated_test.py'
BIN = str(Path(o.binary).resolve())
ROOT = Path(tempfile.mkdtemp(prefix='native-', dir=o.evidence_parent))
ROOT.chmod(448)
home = ROOT / 'state'
home.mkdir()
project = ROOT / 'project'
project.mkdir()
human = ROOT / 'home'
human.mkdir()
runtime = Path(os.environ['JCODE_RUNTIME_DIR'])
sockpath = runtime / 'wp04.sock'
posts = []
events = []
lock = threading.Lock()
counter = 0
proc = None
bridge = None
connections = []
testers = []
hold = threading.Event()

class Provider(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({'data': [{'id': 'fixture', 'context_length': 200000}]}).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        global counter
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with lock:
            counter += 1
            n = counter
            posts.append(body)
            with (ROOT / 'requests.jsonl').open('a') as f:
                f.write(json.dumps(body) + '\n')
        messages = body['messages']
        last = json.dumps(messages[-1])
        tools = [m for m in messages if m.get('role') == 'tool']
        lastid = tools[-1].get('tool_call_id', '') if tools else ''
        parent = any((t.get('function', {}).get('name') == 'subagent' for t in body.get('tools', [])))

        def call(name, args, prefix):
            return ({'role': 'assistant', 'tool_calls': [{'index': 0, 'id': prefix + str(n), 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(dict(args, intent='native fixture'))}}]}, 'tool_calls')
        if 'BOOTSTRAP' in last:
            delta, finish = ({'content': 'BOOTSTRAP OK'}, 'stop')
        elif parent:
            if 'PARENT_BATCH' in last:
                member = {'tool': 'subagent', 'intent': 'independent native child', 'agent': 'fixture', 'model_alias': 'fixture', 'permission': 'read_only', 'prompt': 'SIMPLE CHILD', 'disable_startup_context': True}
                delta, finish = call('batch', {'tool_calls': [member, dict(member)]}, 'p-batch-')
            elif lastid.startswith('p-batch-'):
                delta, finish = ({'content': 'BATCH CHILDREN DONE'}, 'stop')
            elif lastid.startswith('p-child-'):
                delta, finish = ({'content': 'PARENT NATIVE DONE'}, 'stop')
            elif lastid.startswith('p-catalog-'):
                delta, finish = call('subagent', {'agent': 'fixture', 'model_alias': 'fixture', 'permission': 'read_only', 'prompt': 'CHILD TASK', 'disable_startup_context': True}, 'p-child-')
            else:
                delta, finish = call('get_catalog', {}, 'p-catalog-')
        elif 'SIMPLE CHILD' in last:
            delta, finish = ({'content': 'SIMPLE CHILD REPLY'}, 'stop')
        elif 'HOLD CHILD' in last:
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            event = {'id': 'hold', 'object': 'chat.completion.chunk', 'model': 'fixture', 'choices': [{'index': 0, 'delta': {'content': 'HELD PARTIAL'}, 'finish_reason': None}]}
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
            self.wfile.flush()
            hold.wait(120)
            return
        elif 'EXPLICIT CHILD FOLLOWUP' in last:
            delta, finish = ({'content': 'RESTORED CHILD REPLY'}, 'stop')
        elif lastid.startswith('c-env-'):
            delta, finish = ({'content': 'CHILD NATIVE REPLY'}, 'stop')
        elif lastid.startswith('c-write-'):
            delta, finish = call('bash', {'command': 'printf "%s" "${WP04_PARENT_ONLY-unset}"'}, 'c-env-')
        elif lastid.startswith('c-outline-'):
            outline_text = tools[-1]['content']
            if not isinstance(outline_text, str):
                outline_text = ''.join(part.get('text', '') for part in outline_text)
            outline = json.JSONDecoder().raw_decode(outline_text[outline_text.index('{'):])[0]
            reference = next(tool['tool_use_id'] for tool in outline['tools'] if tool['name'] == 'get_catalog')
            delta, finish = call('expand_tool_use', {'snapshot_id': outline['snapshot_id'], 'tool_use_id': reference}, 'c-expand-')
        elif lastid.startswith('c-expand-'):
            text = '\n'.join((m.get('content', '') if isinstance(m.get('content'), str) else json.dumps(m.get('content')) for m in messages))
            match = re.search('<jcode_child_runtime>\\n(.*?)\\n</jcode_child_runtime>', text, re.S)
            if not match:
                raise AssertionError('child runtime notice missing')
            facts = json.loads(match[1])
            target = str(Path(facts['artifact_directory']) / 'native-artifact.txt')
            delta, finish = call('write', {'file_path': target, 'content': 'NATIVE ARTIFACT'}, 'c-write-')
        else:
            delta, finish = call('session_outline', {'target': 'parent'}, 'c-outline-')
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()
        for d, f in [(delta, None), ({}, finish)]:
            event = {'id': 'fixture', 'object': 'chat.completion.chunk', 'created': 0, 'model': 'fixture', 'choices': [{'index': 0, 'delta': d, 'finish_reason': f}]}
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.write(b'data: [DONE]\n\n')
        self.wfile.flush()
http = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
http.daemon_threads = True
threading.Thread(target=http.serve_forever, daemon=True).start()
(home / 'config.toml').write_text(f'[features]\nmemory=false\nswarm=false\n[ambient]\nenabled=false\n[sponsors]\nenabled=false\n[display]\ndebug_socket=true\n[providers.wp04-fixture]\ntype="openai-compatible"\nbase_url="http://127.0.0.1:{http.server_port}/v1"\nauth="none"\nrequires_api_key=false\ndefault_model="fixture"\nmodels=[{{id="fixture",context_window=200000}}]\n')
env = dict(os.environ, HOME=str(human), JCODE_HOME=str(home), JCODE_RUNTIME_DIR=str(runtime), JCODE_SOCKET=str(sockpath), JCODE_DEBUG_CONTROL='1', JCODE_NO_TELEMETRY='1', TERM='xterm-256color')
for key in ['JCODE_SESSION_ID', 'JCODE_CLIENT_SELFDEV', 'JCODE_RUNTIME_PROVIDER', 'JCODE_ACTIVE_PROVIDER', 'WP04_PARENT_ONLY']:
    env.pop(key, None)
base = [BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp04-fixture', '--model', 'fixture', '-C', str(project)]
log = (ROOT / 'server.log').open('ab')
results = {}

def run(args, **kwargs):
    r = subprocess.run(args, env=kwargs.pop('env', env), text=True, capture_output=True, timeout=90, **kwargs)
    with (ROOT / 'commands.jsonl').open('a') as f:
        f.write(json.dumps({'args': args, 'code': r.returncode, 'stdout': r.stdout, 'stderr': r.stderr}) + '\n')
    assert r.returncode == 0, r.stderr[-4000:]
    return r.stdout

def connect():
    c = socket.socket(socket.AF_UNIX)
    c.settimeout(60)
    c.connect(str(sockpath))
    connections.append(c)
    return (c, c.makefile('rb'))

def send(c, value):
    c.sendall((json.dumps(value) + '\n').encode())

def until(r, predicate):
    while True:
        line = r.readline()
        assert line, 'connection closed'
        event = json.loads(line)
        events.append(event)
        if predicate(event):
            return event

def start():
    global proc
    proc = subprocess.Popen(base + ['--socket', str(sockpath), '--debug-socket', 'serve', '--server-name', 'wp04-native'], env=env, stdout=log, stderr=log)
    deadline = time.monotonic() + 45
    while True:
        assert proc.poll() is None, (ROOT / 'server.log').read_text(errors='replace')[-5000:]
        try:
            c, r = connect()
            return (c, r)
        except (FileNotFoundError, ConnectionRefusedError):
            assert time.monotonic() < deadline, 'server startup deadline'
            time.sleep(0.05)

def subscribe(c, r, target=None):
    args = {'type': 'subscribe', 'id': 1, 'working_dir': str(project), 'selfdev': False}
    if target:
        args['target_session_id'] = target
    else:
        args['agent'] = 'fixture'
    send(c, args)
    e = until(r, lambda e: e.get('id') == 1 and e.get('type') in ('done', 'error'))
    assert e['type'] == 'done', e
    return [e['session_id'] for e in events if e.get('type') == 'session'][-1]

def hosted(invocation):
    c, r = connect()
    send(c, {'type': 'delegation_probe', 'id': 71})
    probe = until(r, lambda e: e.get('id') == 71)
    assert probe['version'] == 1, probe
    send(c, {'type': 'delegation_execute', 'id': 72, 'invocation': invocation})
    out = until(r, lambda e: e.get('id') == 72)
    c.close()
    r.close()
    assert out['type'] == 'delegation_result', out
    return out['output']
try:
    run(base + ['run', '--json', 'BOOTSTRAP'])
    (home / 'instructions/agents/fixture.md').write_text('---\nid: fixture\nkind: agent\nname: Fixture\ndescription: Synthetic native fixture\navailability: both\n---\nSYNTHETIC PROFILE')
    (home / 'instructions/system/subagent.md').write_text('---\nid: subagent\nkind: system\n---\nSYNTHETIC CHILD SYSTEM')
    (home / 'instructions/model-roster.toml').write_text('[aliases.fixture]\ndescription="Synthetic local alias"\nmodels=["wp04-fixture:fixture"]\n')
    c, r = start()
    parent = subscribe(c, r)
    def debug(command):
        parts = command.split(':', 2)
        if len(parts) == 3 and parts[1] in testers and parts[2] != 'stop':
            entry = next(entry for entry in json.loads((home / 'testers.json').read_text()) if entry['id'] == parts[1])
            target = Path(entry['debug_cmd_path'])
            response = Path(entry['debug_response_path'])
            operation = {'frame': 'screen-json', 'frame-normalized': 'screen-json-normalized'}.get(parts[2], parts[2])
            if response.exists():
                response.unlink()
            temporary = target.with_suffix('.next')
            temporary.write_text(operation)
            os.replace(temporary, target)
            deadline = time.monotonic() + 15
            while True:
                if response.exists():
                    text = response.read_text()
                    if text:
                        response.unlink()
                        return text
                assert time.monotonic() < deadline, 'Owned tester did not answer ' + operation
                time.sleep(.025)
        return run([BIN, 'debug', command, '--socket', str(sockpath)])
    wrapper = ROOT / 'tester.sh'
    wrapper.write_text('#!/bin/sh\nexec ' + ' '.join(shlex.quote(v) for v in base + ['--socket', str(sockpath), '--resume', parent]) + ' "$@"\n')
    wrapper.chmod(0o700)
    for width in [80, 60]:
        debug('tester:spawn ' + json.dumps({'cwd': str(project), 'binary': str(wrapper), 'cols': width, 'rows': 24}))
        tid = json.loads((home / 'testers.json').read_text())[-1]['id']
        testers.append(tid)
        debug(f'tester:{tid}:wait')
        debug(f'tester:{tid}:keys:esc,esc')
        fixture = debug(f'tester:{tid}:context-editor-fixture:active-child-directive')
        assert 'active-child-directive' in fixture, fixture
        debug(f'tester:{tid}:frame')  # enable capture before an input-triggered repaint
        debug(f'tester:{tid}:keys:space')
        deadline = time.monotonic() + 15
        while True:
            rendered = debug(f'tester:{tid}:frame')
            if 'context_editor' in rendered and 'child' in rendered.lower():
                (ROOT / f'child-lock-{width}x24.json').write_text(rendered)
                break
            assert time.monotonic() < deadline, rendered
            debug(f'tester:{tid}:keys:down,up')
            time.sleep(.1)
        debug(f'tester:{tid}:stop')
        testers.remove(tid)
    results['native_child_directive_frames'] = [80, 60]
    before = len(posts)
    send(c, {'type': 'message', 'id': 2, 'content': 'PARENT TASK', 'no_reply': False})
    e = until(r, lambda e: e.get('id') == 2 and e.get('type') in ('done', 'error'))
    assert e['type'] == 'done', e
    assert len(posts) - before == 8, (len(posts) - before, events[-10:])
    assert any(('CHILD NATIVE REPLY' in json.dumps(m) for m in posts[-1]['messages']))
    children = list((home / 'sessions').glob('session_child_*.json'))
    assert len(children) == 1
    child = json.loads(children[0].read_text())
    child_id = child['id']
    frozen = child['system_prompt']
    resolution = child['isolated_child']['identity']['resolution']
    assert (Path(child['isolated_child']['identity']['artifact_dir']) / 'native-artifact.txt').read_text() == 'NATIVE ARTIFACT'
    results['daemon_parent_waits_child_inspects_and_writes'] = True
    db = sqlite3.connect(f'file:{home}/execution/index.sqlite?mode=ro', uri=True)
    row = db.execute("SELECT input_path FROM runs WHERE session_id=? AND tool='subagent'", (parent,)).fetchone()
    db.close()
    invocation = json.loads(Path(row[0]).read_text())
    invocation = {k: invocation[k] for k in ['session_id', 'message_id', 'call_path', 'working_dir', 'tool', 'input']}
    before = len(posts)
    out = hosted(invocation)
    assert not out['is_error'] and len(posts) == before
    results['transport_replay_does_not_repeat_child'] = True
    # Explicit permission/preset transition and sticky omission through the real
    # host. Synthetic prose checks only placement and exact snapshot retention.
    (home / 'instructions/notifications/task-preset.native.md').write_text('---\nid: task-preset.native\nkind: notification\nname: Native\ndescription: Synthetic native preset\n---\nSYNTHETIC PRESET')
    baseline_messages = child['messages']
    before = len(posts)
    changed = hosted({'session_id': parent, 'message_id': 'settings-change', 'call_path': ['settings-change'], 'working_dir': str(project), 'tool': 'subagent', 'input': {'child_id': child_id, 'prompt': 'EXPLICIT CHILD FOLLOWUP', 'permission': 'read_write', 'preset': 'native', 'intent': 'fixture'}})
    assert not changed['is_error'], changed
    changed_state = json.loads(children[0].read_text())
    controls = changed_state['isolated_child']
    assert controls['permission'] == 'read_write' and controls['preset']['id'] == 'task-preset.native'
    assert changed_state['system_prompt'] == frozen
    assert changed_state['messages'][:len(baseline_messages)] == baseline_messages
    omitted = hosted({'session_id': parent, 'message_id': 'settings-omitted', 'call_path': ['settings-omitted'], 'working_dir': str(project), 'tool': 'subagent', 'input': {'child_id': child_id, 'prompt': 'EXPLICIT CHILD FOLLOWUP', 'intent': 'fixture'}})
    assert not omitted['is_error'], omitted
    sticky = json.loads(children[0].read_text())['isolated_child']
    assert sticky['permission_message_id'] == controls['permission_message_id'] and sticky['preset_message_id'] == controls['preset_message_id']
    assert len(posts) == before + 2
    results['changed_settings_then_omission_preserve_prefix_and_directives'] = True
    before = len(posts)
    send(c, {'type': 'message', 'id': 3, 'content': 'PARENT_BATCH', 'no_reply': False})
    batch_done = until(r, lambda e: e.get('id') == 3 and e.get('type') in ('done', 'error'))
    assert batch_done['type'] == 'done', batch_done
    assert len(posts) == before + 4, 'Independent child batch did not finish exactly once'
    with sqlite3.connect(f'file:{home}/execution/index.sqlite?mode=ro', uri=True) as db:
        batch_id = db.execute("SELECT id FROM runs WHERE session_id=? AND tool='batch'", (parent,)).fetchone()[0]
        members = db.execute("SELECT r.id,c.child_id,r.state FROM runs r JOIN child_turns c ON c.run_id=r.id WHERE r.parent_id=?", (batch_id,)).fetchall()
    assert len(members) == 2 and len({row[1] for row in members}) == 2
    assert all(row[2] == 'completed' for row in members)
    results['independent_child_batch_has_distinct_retained_members'] = True
    for flag in ['--json', '--ndjson']:
        before = len(posts)
        text = run(base + ['--agent', 'fixture', 'run', flag, 'PARENT TASK'], env=dict(env, WP04_PARENT_ONLY='must-not-forward'))
        if flag == '--json':
            json.loads(text)
        else:
            for line in text.splitlines():
                json.loads(line)
        assert len(posts) - before == 8, (flag, len(posts) - before)
        assert any(('unset' in json.dumps(m) for m in posts[-2]['messages'])), 'parent-only environment reached child'
        results['standalone_' + flag[2:]] = True
    before = len(posts)
    text = run(base + ['--agent', 'fixture', 'repl'], input='PARENT TASK\nexit\n', env=dict(env, WP04_PARENT_ONLY='must-not-forward'))
    assert 'PARENT NATIVE DONE' in text and len(posts) - before == 8
    results['direct_repl'] = True
    api = runtime / 'wp04-api.sock'
    bridge = subprocess.Popen([BIN, '--no-update', 'api-bridge'], env=dict(env, JCODE_API_SOCKET=str(api)), stdout=log, stderr=log)
    deadline = time.monotonic() + 30
    while not api.exists():
        assert bridge.poll() is None and time.monotonic() < deadline, 'API bridge startup failed'
        time.sleep(0.05)
    js = ROOT / 'sdk.mjs'
    js.write_text('import { JcodeClient } from ' + json.dumps(Path(o.sdk_entry).resolve().as_uri()) + ";\nconst client=await JcodeClient.connect({socketPath:process.argv[2],requestTimeoutMs:30000});\ntry { const session=await client.createSession(process.argv[3],'fixture'); const result=await client.run(session.session_id,'PARENT TASK',{autoApprove:true}); if(!result.text.includes('PARENT NATIVE DONE') || !result.toolCalls.some(t=>t.name==='subagent')) throw Error(JSON.stringify(result)); console.log(JSON.stringify({session:session.session_id,tools:result.toolCalls.map(t=>t.name)})); } finally { client.close(); }\n")
    before = len(posts)
    sdk = json.loads(run(['node', str(js), str(api), str(project)]))
    assert len(posts) - before == 8
    results['typescript_sdk_harness'] = sdk
    global_script = ROOT / 'global-stream.mjs'
    global_script.write_text('import { JcodeClient } from ' + json.dumps(Path(o.sdk_entry).resolve().as_uri()) + ";\n" + """
const client = await JcodeClient.connect({socketPath: process.argv[2], requestTimeoutMs: 30000});
try {
  const sessions = await client.listSessions({includeArchived: true});
  const children = new Set(sessions.filter(s => s.attachable === false).map(s => s.session_id));
  if (!children.size) throw Error('Child attachment eligibility was not exposed');
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 1500);
  try {
    for await (const event of client.globalEvents({signal: abort.signal, discoveryIntervalMs: 50})) {
      if (children.has(event.session_id)) throw Error('Child chat was attached by globalEvents');
    }
  } finally { clearTimeout(timer); }
  console.log(JSON.stringify({nonAttachableChildren: children.size}));
} finally { await client.close(); }
""")
    before = len(posts)
    results['sdk_global_events_preserves_primary_streams_with_children'] = json.loads(run(['node', str(global_script), str(api)]))
    assert len(posts) == before, 'Event discovery started inference'
    bridge.terminate()
    bridge.wait(timeout=20)
    bridge = None
    before = len(posts)
    accepted = hosted({'session_id': parent, 'message_id': 'native-hold', 'call_path': ['native-hold'], 'working_dir': str(project), 'tool': 'subagent', 'input': {'child_id': child_id, 'prompt': 'HOLD CHILD', 'run_in_background': True, 'notify': False, 'intent': 'fixture'}})
    assert not accepted['is_error'], accepted
    queued = hosted({'session_id': parent, 'message_id': 'native-queued', 'call_path': ['native-queued'], 'working_dir': str(project), 'tool': 'subagent', 'input': {'child_id': child_id, 'prompt': 'UNSTARTED INPUT', 'queue_if_busy': True, 'run_in_background': True, 'notify': False, 'intent': 'fixture'}})
    assert not queued['is_error'], queued
    deadline = time.monotonic() + 30
    while True:
        db = sqlite3.connect(f'file:{home}/execution/index.sqlite?mode=ro', uri=True)
        row = db.execute("SELECT output_path FROM runs WHERE session_id=? AND message_id='native-hold'", (parent,)).fetchone()
        db.close()
        if row and row[0] and ('HELD PARTIAL' in Path(row[0]).read_text()):
            break
        assert time.monotonic() < deadline, 'partial output not retained'
        time.sleep(0.05)
    r.close()
    c.close()
    proc.kill()
    proc.wait(timeout=20)
    c, r = start()
    assert subscribe(c, r, parent) == parent
    out = hosted({'session_id': parent, 'message_id': 'after-crash', 'call_path': ['after-crash'], 'working_dir': str(project), 'tool': 'subagent', 'input': {'child_id': child_id, 'prompt': 'EXPLICIT CHILD FOLLOWUP', 'intent': 'fixture'}})
    assert not out['is_error'], out
    db = sqlite3.connect(f'file:{home}/execution/index.sqlite?mode=ro', uri=True)
    states = dict(db.execute("SELECT message_id,state FROM runs WHERE session_id=? AND message_id IN ('native-hold','native-queued')", (parent,)))
    db.close()
    assert states == {'native-hold': 'interrupted', 'native-queued': 'cancelled'}, states
    assert len(posts) == before + 2, 'crash recovery replayed model work'
    results['active_child_crash_queue_and_no_replay'] = True
    hold.set()
    r.close()
    c.close()
    proc.terminate()
    proc.wait(timeout=20)
    before = len(posts)
    try:
        text = run(base + ['--agent', 'fixture', 'run', '--json', 'PARENT TASK'], env=dict(env, WP04_PARENT_ONLY='must-not-forward'))
        json.loads(text)
        assert len(posts) - before == 8, 'autostart did not execute the complete child workflow'
        assert any(('unset' in json.dumps(m) for m in posts[-2]['messages'])), 'autostart copied an arbitrary parent export'
        results['host_autostart'] = True
    finally:
        ownership = subprocess.run(['/usr/sbin/lsof', '-t', str(sockpath)], text=True, capture_output=True)
        pids = {int(pid) for pid in ownership.stdout.split()}
        for pid in pids:
            before_image = subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart='], text=True).strip()
            again = subprocess.run(['/usr/sbin/lsof', '-t', str(sockpath)], text=True, capture_output=True)
            if str(pid) in again.stdout.split() and subprocess.check_output(['ps', '-p', str(pid), '-o', 'lstart='], text=True).strip() == before_image:
                os.kill(pid, signal.SIGTERM)
                deadline = time.monotonic() + 20
                while True:
                    state = subprocess.run(['ps', '-p', str(pid), '-o', 'state=,lstart='], text=True, capture_output=True).stdout.strip()
                    if not state or state.startswith('Z') or before_image not in state:
                        break
                    assert time.monotonic() < deadline, 'Owned autostart daemon did not exit'
                    time.sleep(.05)
    c, r = start()
    assert subscribe(c, r, parent) == parent
    (home / 'instructions/model-roster.toml').write_text('broken roster = [')
    (home / 'instructions/agents/fixture.md').write_text('broken profile')
    r.close()
    c.close()
    proc.terminate()
    proc.wait(timeout=20)
    c, r = start()
    assert subscribe(c, r, parent) == parent
    before = len(posts)
    out = hosted({'session_id': parent, 'message_id': 'native-followup', 'call_path': ['native-followup'], 'working_dir': str(project), 'tool': 'subagent', 'input': {'child_id': child_id, 'prompt': 'EXPLICIT CHILD FOLLOWUP', 'intent': 'fixture'}})
    assert not out['is_error'] and 'RESTORED CHILD REPLY' in out['output'], out
    assert len(posts) == before + 1
    restored = json.loads(children[0].read_text())
    assert restored['system_prompt'] == frozen and restored['isolated_child']['identity']['resolution'] == resolution
    results['restart_retains_exact_child_and_ignores_edited_alias_profile'] = True
    results['binary'] = run([BIN, '--version']).strip()
    results['provider_requests'] = len(posts)
    results['root'] = str(ROOT)
finally:
    for tid in testers:
        try:
            run([BIN, 'debug', f'tester:{tid}:stop', '--socket', str(sockpath)])
        except Exception:
            pass
    hold.set()
    if bridge and bridge.poll() is None:
        bridge.terminate()
        bridge.wait(timeout=20)
    for c in connections:
        try:
            c.close()
        except OSError:
            pass
    if proc and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    http.shutdown()
    http.server_close()
    log.close()
    (ROOT / 'events.json').write_text(json.dumps(events, indent=2))
    (ROOT / 'results.json').write_text(json.dumps(results, indent=2))
    print(json.dumps({'root': str(ROOT), 'results': results}, indent=2))
