#!/usr/bin/env python3
"""Read-only mechanism acceptance using the production daemon and PTY TUI.

Creates private disposable state and local HTTP fixtures. Never sends paid
model requests, changes real instruction stores, or operates a real account.
Artifacts remain under --artifact-dir; owned daemons and testers are stopped.
"""
import argparse, hashlib, http.server, json, os, shlex, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path
parser = argparse.ArgumentParser(description='Sandboxed read-only instruction manager acceptance. All model endpoints are local fixtures.')
parser.add_argument('--binary', default=str(Path.home() / '.jcode/builds/current/jcode'))
parser.add_argument('--artifact-dir', default=str(Path(os.environ.get('JCODE_SCRATCH_DIR', str(Path.home() / '.jcode/scratch'))) / 'instruction-manager'))
options = parser.parse_args()
BASE = Path(options.artifact_dir)
BASE.mkdir(parents=True, exist_ok=True)
ROOT = Path(tempfile.mkdtemp(prefix='live-', dir=BASE))
(ROOT / 'probe.py').write_text(Path(__file__).read_text())
BIN = str(Path(options.binary).resolve())
posts = []

class FixtureProvider(http.server.BaseHTTPRequestHandler):

    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({'object': 'list', 'data': [{'id': 'fixture', 'object': 'model', 'owned_by': 'fixture'}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        posts.append(self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode())
        self.send_response(500)
        self.end_headers()
        self.wfile.write(b'Inspection must not call a model')
http = http.server.ThreadingHTTPServer(('127.0.0.1', 0), FixtureProvider)
threading.Thread(target=http.serve_forever, daemon=True).start()
home = ROOT / 'home'
home.mkdir()
project = ROOT / 'project'
project.mkdir()
sockpath = ROOT / 's.sock'
(home / 'config.toml').write_text(f'[features]\nmemory=false\nswarm=false\n[ambient]\nenabled=false\n[sponsors]\nenabled=false\n[display]\ndebug_socket=true\n[providers.wp09-fixture]\ntype="openai-compatible"\nbase_url="http://127.0.0.1:{http.server_port}/v1"\napi_key_env="WP09_FIXTURE_KEY"\ndefault_model="fixture"\n')
env = os.environ.copy()
env.update(JCODE_HOME=str(home), JCODE_RUNTIME_DIR=str(ROOT / 'runtime'), JCODE_SOCKET=str(sockpath), JCODE_DEBUG_CONTROL='1', JCODE_NO_TELEMETRY='1', WP09_FIXTURE_KEY='synthetic-local-key', TERM='xterm-256color', GIT_OPTIONAL_LOCKS='0')
args = [BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', '--socket', str(sockpath), '-C', str(project), '--debug-socket', 'serve', '--server-name', 'wp09-validation']
proc = None
client = None
reader = None
testers = []
events = []
counter = 20
log = open(ROOT / 'server.log', 'wb')

def start():
    global proc, client, reader
    proc = subprocess.Popen(args, env=env, stdout=log, stderr=log)
    deadline = time.monotonic() + 30
    while True:
        if proc.poll() is not None:
            raise RuntimeError('daemon exited: ' + (ROOT / 'server.log').read_text(errors='replace')[-3000:])
        client = socket.socket(socket.AF_UNIX)
        client.settimeout(30)
        try:
            client.connect(str(sockpath))
            break
        except (FileNotFoundError, ConnectionRefusedError):
            client.close()
            if time.monotonic() > deadline:
                raise
            time.sleep(0.1)
    reader = client.makefile('rb')

def send(value):
    client.sendall((json.dumps(value) + '\n').encode())

def until(predicate):
    while True:
        line = reader.readline()
        if not line:
            raise RuntimeError('daemon closed connection')
        event = json.loads(line)
        events.append(event)
        if predicate(event):
            return event

def subscribe(session=None):
    global counter
    counter += 1
    value = {'type': 'subscribe', 'id': counter, 'working_dir': str(project), 'selfdev': False}
    if session:
        value['target_session_id'] = session
    else:
        value['agent'] = 'global:jcode'
    send(value)
    result = until(lambda e: e.get('id') == counter and e.get('type') in ('done', 'error'))
    assert result['type'] == 'done', result
    actual = [e['session_id'] for e in events if e.get('type') == 'session'][-1]
    if session:
        assert actual == session
    return actual

def inspect(request):
    global counter
    counter += 1
    request_id = counter
    send({'type': 'inspect_instructions', 'id': request_id, 'request': request})
    event = until(lambda e: e.get('id') == request_id and e.get('type') in ('instruction_inspection', 'error'))
    assert event['type'] == 'instruction_inspection', event
    return event['reply']

def request_data(request, expected):
    reply = inspect(request)
    assert reply['result']['result'] == expected, reply
    return reply['result']['data']

def open_snapshot():
    return request_data({'operation': 'open', 'filter': {'search': ''}}, 'opened')

def rows(snapshot):
    page = snapshot['resources']
    result = list(page['rows'])
    while page['next'] is not None:
        page = request_data({'operation': 'resources', 'snapshot': snapshot['snapshot'], 'filter': {'search': ''}, 'offset': page['next']}, 'resources')
        result += page['rows']
    assert len(result) == page['total']
    return result

def target(row):
    return {'target': 'resource', 'key': row['key']}

def detail(snapshot, row, view='source', revision=None):
    return request_data({'operation': 'detail', 'snapshot': snapshot['snapshot'], 'target': target(row), 'view': view, 'revision': revision}, 'text')

def complete(snapshot, page):
    data = page['text'].encode()
    pages = 1
    while page['next'] is not None:
        page = request_data({'operation': 'text', 'snapshot': snapshot['snapshot'], 'document': page['document'], 'offset': page['next']}, 'text')
        assert page['offset'] == len(data)
        data += page['text'].encode()
        pages += 1
    assert len(data) == page['total_bytes']
    return (data.decode(), pages)

def source(root, directory, name, kind, body, extra=''):
    path = root / directory / (name + '.md')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f'---\nid: {name}\nkind: {kind}\n{extra}---\n' + body)
    return path

def git(root, *args):
    result = subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@localhost', *args], cwd=root, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (args, result.stderr)
    return result.stdout.strip()

def fingerprints(root):
    values = {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest() for path in root.rglob('*') if path.is_file() and '.git' not in path.relative_to(root).parts}
    values['.git/index'] = hashlib.sha256((root / '.git/index').read_bytes()).hexdigest()
    values['HEAD'] = git(root, 'rev-parse', 'HEAD')
    values['status'] = git(root, 'status', '--porcelain=v2', '--untracked-files=all')
    return values

def debug(command):
    result = subprocess.run([BIN, 'debug', command, '--socket', str(sockpath)], env=env, capture_output=True, text=True, timeout=25)
    with (ROOT / 'debug.jsonl').open('a') as out:
        out.write(json.dumps({'command': command, 'code': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}) + '\n')
    assert result.returncode == 0, (command, result.stdout, result.stderr)
    return result.stdout

def tester_state(tid, predicate=lambda state: True):
    deadline = time.monotonic() + 25
    while True:
        result = debug(f'tester:{tid}:instruction-manager')
        try:
            state = json.loads(result)
        except json.JSONDecodeError:
            state = {}
        if predicate(state):
            return state
        if time.monotonic() > deadline:
            raise TimeoutError('manager state did not converge: ' + result)
        time.sleep(0.1)

def frame(tid, expected_text='WORKING', motion='end'):
    """Wait for a fresh selected-source viewport. Legacy tester IPC may acknowledge a key before rendering. End is idempotent for this one-page fixture."""
    first = debug(f'tester:{tid}:frame')
    try:
        first_id = json.loads(first).get('frame_id', -1)
    except json.JSONDecodeError:
        first_id = -1
    deadline = time.monotonic() + 15
    while True:
        debug(f'tester:{tid}:keys:{motion}')
        time.sleep(0.08)
        result = debug(f'tester:{tid}:frame')
        try:
            value = json.loads(result)
        except json.JSONDecodeError:
            value = {}
        viewport = '\n'.join((row.get('content_preview', '') for row in value.get('rendered_text', {}).get('recent_messages', [])))
        if value.get('frame_id', -1) >= first_id and 'instruction_manager' in value.get('render_order', []) and (expected_text in viewport):
            return value
        if time.monotonic() > deadline:
            raise TimeoutError('fresh selected-source frame not captured: ' + result)
        time.sleep(0.08)
if __name__ == "__main__":
    try:
        start()
        session = subscribe()
        store = home / 'instructions'
        small = source(store, 'modules', 'wp09-small', 'module', 'FIRST')
        agent = source(store, 'agents', 'wp09-agent', 'agent', '{{>wp09-small}}', 'name: Inspection Fixture\ndescription: Synthetic fixture\navailability: both\ntemplate: handlebars\n')
        large_text = '界α🙂\n' * 90000
        large = source(store, 'modules', 'wp09-large', 'module', large_text)
        (store / 'model-roster.toml').write_text('[aliases.fixture]\ndescription="Synthetic"\nmodels=["openai-compatible:wp09-fixture:fixture"]\nnotes="HUMAN ONLY"\n[aliases.invalid]\ndescription="Invalid fixture"\nmodels=[]\n')
        git(store, 'add', '.')
        git(store, 'commit', '-m', 'inspection fixture first')
        source(store, 'modules', 'wp09-small', 'module', 'SECOND')
        git(store, 'add', 'modules/wp09-small.md')
        git(store, 'commit', '-m', 'inspection fixture second')
        source(store, 'modules', 'wp09-small', 'module', 'WORKING')
        project_store = project / 'instructions'
        project_store.mkdir()
        git(project_store, 'init', '-b', 'main')
        (project_store / 'instruction-store.toml').write_text('schema_version=1\nseed_version=27\n')
        source(project_store, 'agents', 'wp09-agent', 'agent', 'PROJECT COMPONENT', 'name: Project Fixture\ndescription: Synthetic\navailability: both\n')
        source(project_store, 'system', 'common', 'system', 'PROJECT COMMON')
        source(project_store, 'modules', 'broken', 'module', '{{>missing}}', 'template: handlebars\n')
        (project / '.jcode').mkdir(exist_ok=True)
        (project / '.jcode/instructions.toml').write_text('[repository]\nmode="standalone"\npath="instructions"\n')
        config = project / '.jcode/instructions.toml'
        config.write_text('schema_version=1\n' + config.read_text())
        (project / 'AGENTS.md').write_text('PROJECT ECOSYSTEM')
        (home / 'prompt-overlay.md').write_text('INACTIVE GLOBAL LEGACY')
        ext = project / '.jcode/skills/external'
        ext.mkdir(parents=True)
        (ext / 'SKILL.md').write_text('---\nname: External fixture\ndescription: Synthetic\n---\nEXTERNAL BODY')
        bad = project / '.jcode/skills/bad'
        bad.mkdir()
        (bad / 'SKILL.md').write_bytes(b'\xff')
        git(project_store, 'add', '.')
        git(project_store, 'commit', '-m', 'project fixture')
        baseline = {str(root): fingerprints(root) for root in [store, project_store]}
        saved_before = json.loads((home / 'sessions' / (session + '.json')).read_text())
        snapshot = open_snapshot()
        catalog = rows(snapshot)
        (ROOT / 'catalog.json').write_text(json.dumps(catalog, indent=2))
        assert any((r['scope'] == 'project' and (not r['valid']) for r in catalog))
        assert any((r['origin'] == 'external' and r['kind'] == 'skill' and (not r['valid']) for r in catalog))
        assert any((r['id'] == 'wp09-agent' and r['scope'] == 'global' and (not r['effective']) for r in catalog))
        grouped = request_data({'operation': 'resources', 'snapshot': snapshot['snapshot'], 'filter': {'search': '', 'redefinitions': True}, 'offset': 0}, 'resources')
        assert grouped['rows'] and all((row['redefines_global'] for row in grouped['rows']))
        assert any((row['high_impact'] for row in grouped['rows']))
        assert any((r['kind'] == 'legacy-prompt' and (not r['effective']) for r in catalog))
        large_row = next((r for r in catalog if r['id'] == 'wp09-large'))
        original = large.read_text()
        page = detail(snapshot, large_row)
        large.write_text('changed during captured paging')
        reconstructed, pages = complete(snapshot, page)
        assert reconstructed == original and pages > 1
        large.write_text(original)
        project_agent = next((r for r in catalog if r['id'] == 'wp09-agent' and r['scope'] == 'project'))
        rendered, _ = complete(snapshot, detail(snapshot, project_agent, 'system'))
        assert 'PROJECT COMPONENT' in rendered and 'PROJECT COMMON' in rendered
        scoped, _ = complete(snapshot, detail(snapshot, project_agent, 'scope_comparison'))
        assert 'PROJECT COMPONENT' in scoped and '{{>wp09-small}}' in scoped
        global_agent = next((r for r in catalog if r['id'] == 'wp09-agent' and r['scope'] == 'global'))
        graph, _ = complete(snapshot, detail(snapshot, global_agent, 'dependencies'))
        assert 'wp09-small' in graph
        small_row = next((r for r in catalog if r['id'] == 'wp09-small'))
        working, _ = complete(snapshot, detail(snapshot, small_row, 'working_diff'))
        assert '+WORKING' in working
        history = request_data({'operation': 'history', 'snapshot': snapshot['snapshot'], 'target': target(small_row), 'offset': 0}, 'history')
        assert len(history['commits']) == 2
        historical, _ = complete(snapshot, detail(snapshot, small_row, revision={'from': history['commits'][1]['commit']}))
        assert historical.endswith('FIRST')
        difference, _ = complete(snapshot, detail(snapshot, small_row, revision={'from': history['commits'][1]['commit'], 'to': history['commits'][0]['commit']}))
        assert '+SECOND' in difference
        roster_row = next((r for r in catalog if r['id'] == 'fixture' and r['kind'] == 'model-roster'))
        roster_preview, _ = complete(snapshot, detail(snapshot, roster_row, 'rendered'))
        (ROOT / 'roster-preview.txt').write_text(roster_preview)
        assert 'fixture' in roster_preview
        stored = request_data({'operation': 'detail', 'snapshot': snapshot['snapshot'], 'target': {'target': 'session'}, 'view': 'system'}, 'text')
        stored_text, _ = complete(snapshot, stored)
        assert stored_text == saved_before['system_prompt']['text']
        stale = inspect({'operation': 'text', 'snapshot': 'wrong-snapshot', 'document': stored['document'], 'offset': 0})
        assert stale['result']['result'] == 'failed'
        denied = inspect({'operation': 'detail', 'snapshot': snapshot['snapshot'], 'target': {'target': 'resource', 'key': '/etc/passwd'}, 'view': 'source'})
        assert denied['result']['result'] == 'failed'
        assert not posts
        assert baseline == {str(root): fingerprints(root) for root in [store, project_store]}
        saved_after = json.loads((home / 'sessions' / (session + '.json')).read_text())
        for key in ['system_prompt', 'active_skill', 'messages', 'model', 'reasoning_effort', 'route_api_method']:
            assert saved_before.get(key) == saved_after.get(key), key
        print('JCODE_PROGRESS ' + json.dumps({'message': 'Read-only API, complete paging, Git and provider-state checks passed', 'current': 1, 'total': 6}), flush=True)
        wrapper = ROOT / 'tester.sh'
        wrapper.write_text('#!/bin/sh\nexec ' + shlex.quote(BIN) + ' --no-update --no-selfdev --provider-profile wp09-fixture --model fixture --socket ' + shlex.quote(str(sockpath)) + ' --resume ' + shlex.quote(session) + ' "$@"\n')
        wrapper.chmod(448)
        frames = []
        for cols, lines in [(150, 40), (80, 24), (60, 24), (24, 10)]:
            debug('tester:spawn ' + json.dumps({'cwd': str(project), 'binary': str(wrapper), 'cols': cols, 'rows': lines}))
            tid = json.loads((home / 'testers.json').read_text())[-1]['id']
            testers.append(tid)
            debug(f'tester:{tid}:wait')
            debug(f'tester:{tid}:keys:esc,esc')
            ui_before = json.loads((home / 'sessions' / (session + '.json')).read_text())
            debug(f'tester:{tid}:set_input:/instructions')
            debug(f'tester:{tid}:keys:enter')
            state = tester_state(tid, lambda s: s.get('visible') and s.get('rows_loaded', 0) > 0 and (s.get('pending_id') is None))
            assert state['layout'] in ('wide', 'tabs')
            debug(f'tester:{tid}:keys:g')
            tester_state(tid, lambda s: s.get('rows_loaded') == 1 and s.get('pending_id') is None)
            group_frame = frame(tid, '[!]')
            (ROOT / f'group-{cols}x{lines}.json').write_text(json.dumps(group_frame, indent=2))
            debug(f'tester:{tid}:keys:c')
            tester_state(tid, lambda s: s.get('rows_loaded', 0) > 1 and s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:/,w,p,0,9,-,s,m,a,l,l,enter')
            tester_state(tid, lambda s: s.get('rows_loaded') == 1 and s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:1')
            tester_state(tid, lambda s: s.get('section') == 'Source' and s.get('detail_bytes_loaded', 0) > 0 and (s.get('pending_id') is None))
            assert 'WORKING' not in debug(f'tester:{tid}:instruction-manager')
            debug(f'tester:{tid}:keys:/,ctrl+u,w,p,0,9,-,l,a,r,g,e,enter')
            tester_state(tid, lambda s: s.get('resource_id') == 'wp09-large' and s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:1')
            tester_state(tid, lambda s: s.get('detail_bytes_loaded', 0) > 1000 and s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:/,ctrl+u,w,p,0,9,-,s,m,a,l,l,enter')
            tester_state(tid, lambda s: s.get('resource_id') == 'wp09-small' and s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:1')
            tester_state(tid, lambda s: 0 < s.get('detail_bytes_loaded', 0) < 1000 and s.get('pending_id') is None)
            image = frame(tid)
            frames.append(image)
            (ROOT / f'frame-{cols}x{lines}.json').write_text(json.dumps(image, indent=2))
            control=next(item['rect'] for item in image['layout']['widget_placements'] if item['kind']=='instruction-action-F1')
            debug(f"tester:{tid}:mouse:click:{control['x']},{control['y']}")
            tester_state(tid, lambda s: s.get('pane') == 'Repositories')
            debug(f'tester:{tid}:keys:f2')
            debug(f'tester:{tid}:keys:space')
            tester_state(tid,lambda state:state.get('menu')=='Actions')
            actions_frame=frame(tid,'Actions',motion='home')
            (ROOT/f'actions-{cols}x{lines}.json').write_text(json.dumps(actions_frame,indent=2))
            debug(f'tester:{tid}:keys:o,v,e,r,v,i,e,w,enter')
            tester_state(tid,lambda state:state.get('section')=='Metadata' and state.get('pending_id') is None and state.get('menu') is None)
            overview_frame=frame(tid,'WORKING',motion='home')
            (ROOT/f'overview-{cols}x{lines}.json').write_text(json.dumps(overview_frame,indent=2))
            debug(f'tester:{tid}:keys:esc')
            tester_state(tid,lambda state:state.get('pane')=='Resources')
            debug(f'tester:{tid}:keys:f')
            tester_state(tid,lambda state:state.get('menu')=='Filters')
            debug(f'tester:{tid}:keys:s,c,o,p,e,enter')
            tester_state(tid,lambda state:state.get('menu')=='Source scope')
            filter_frame=frame(tid,'Source scope',motion='home')
            (ROOT/f'filters-{cols}x{lines}.json').write_text(json.dumps(filter_frame,indent=2))
            debug(f'tester:{tid}:keys:g,l,o,b,a,l,enter')
            tester_state(tid,lambda state:state.get('scope')=='global' and state.get('pending_id') is None)
            debug(f'tester:{tid}:keys:6')
            tester_state(tid,lambda state:state.get('section')=='History' and state.get('pending_id') is None)
            debug(f'tester:{tid}:keys:a,down,b')
            tester_state(tid,lambda state:state.get('detail_view')=='Revision comparison' and state.get('pending_id') is None)
            comparison_frame=frame(tid,'FIRST',motion='down')
            (ROOT/f'comparison-{cols}x{lines}.json').write_text(json.dumps(comparison_frame,indent=2))
            debug(f'tester:{tid}:keys:esc')
            tester_state(tid,lambda state:state.get('section')=='History')
            debug(f'tester:{tid}:keys:i')
            tester_state(tid,lambda state:state.get('detail_view')=='Commit details' and state.get('pending_id') is None)
            commit_frame=frame(tid,'COMMIT DETAILS',motion='home')
            (ROOT/f'commit-{cols}x{lines}.json').write_text(json.dumps(commit_frame,indent=2))
            debug(f'tester:{tid}:keys:esc,esc')
            tester_state(tid,lambda state:state.get('pane')=='Resources')
            debug(f'tester:{tid}:keys:r')
            tester_state(tid, lambda s: s.get('rows_loaded') == 1 and s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:x')
            tester_state(tid, lambda s: s.get('pending_id') is None)
            debug(f'tester:{tid}:keys:q')
            tester_state(tid, lambda s: not s.get('visible'))
            ui_after = json.loads((home / 'sessions' / (session + '.json')).read_text())
            for key in ['system_prompt', 'active_skill', 'messages', 'model', 'reasoning_effort', 'route_api_method']:
                assert ui_before.get(key) == ui_after.get(key), key
            debug(f'tester:{tid}:stop')
            testers.remove(tid)
            print('JCODE_PROGRESS ' + json.dumps({'message': f'Physical TUI keys, mouse and fresh source frame passed at {cols}x{lines}', 'current': len(frames) + 1, 'total': 6}), flush=True)
        assert not posts
        assert baseline == {str(root): fingerprints(root) for root in [store, project_store]}
        client.close()
        client = None
        proc.terminate()
        proc.wait(timeout=15)
        start()
        assert subscribe(session) == session
        fresh = open_snapshot()
        assert fresh['snapshot'] != snapshot['snapshot']
        old = inspect({'operation': 'text', 'snapshot': snapshot['snapshot'], 'document': stored['document'], 'offset': 0})
        assert old['result']['result'] == 'failed'
        assert not posts
        result = {'binary': subprocess.check_output([BIN, '--version'], text=True).strip(), 'catalog_rows': len(catalog), 'complete_content_pages': pages, 'source_and_index_unchanged': True, 'session_instructions_and_messages_unchanged': True, 'provider_requests': len(posts), 'actual_remote_keys_mouse_frames': [[150, 40], [80, 24], [60, 24], [24, 10]], 'ux_named_actions_explicit_filters_history_back_and_sticky_context':True, 'server_restart_reconnect_and_stale_rejection': True, 'artifact': str(ROOT)}
        (ROOT / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result))
    finally:
        (ROOT / 'events.json').write_text(json.dumps(events, indent=2))
        (ROOT / 'provider-posts.json').write_text(json.dumps(posts, indent=2))
        for tid in list(testers):
            try:
                debug(f'tester:{tid}:stop')
            except Exception:
                pass
        if client:
            client.close()
        if proc and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        http.shutdown()
        log.close()
        print('artifacts=' + str(ROOT))
