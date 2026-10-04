#!/usr/bin/env python3
"""Combined C01 organization journey (SP-58-C01/WP-12, J01 and the related
parts of J02/J04) over the real daemon with managed launch rolled out.

  - project with two logical repositories, a non-Git directory reference and a
    flat work area; one independent clone made intentionally in the area
  - two sessions launched in that clone share it: one checkout identity, no
    second clone, both bound to the same location
  - a project-level session needs an explicit cwd and then uses one
  - a projectless standalone session gets no invented project
  - initial location facts name the placement and its home chain
  - archiving the project hides idle rows but keeps active work visible and
    usable: an archived session still runs a turn; unarchive restores the rest

All state is private and disposable; the provider is a localhost script.
Run through scripts/run_isolated_test.py with --binary and --artifact-dir.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, subprocess, sys, time, traceback, uuid
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_instruction_manager as f

config = f.home / 'config.toml'
config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))
posts = []
def provider(self):
    posts.append(self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode())
    chunks = [({'content': 'fixture reply'}, None), ({}, 'stop')]
    data = (''.join('data: ' + json.dumps({'id': 'c', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': d, 'finish_reason': r}]}) + '\n\n' for d, r in chunks) + 'data: [DONE]\n\n').encode()
    self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
f.FixtureProvider.do_POST = provider

ROOT = f.ROOT
evidence = ROOT / 'combined-evidence'; evidence.mkdir()
steps = []
result = {'status': 'failed'}
def step(name): steps.append({'step': name, 'at': time.time(), 'posts': len(posts)}); print('step:', name, flush=True)
def uid(): return str(uuid.uuid4())
def wait(check, label, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value: return value
        time.sleep(0.1)
    raise TimeoutError(label)
def rpc(kind, **fields):
    f.counter += 1; ident = f.counter
    f.send({'type': kind, 'id': ident, **fields})
    return f.until(lambda e: e.get('id') == ident and e.get('type') != 'ack')
def ws(action, kind, **fields):
    event = rpc('workspace', request={'action': action, **fields})
    assert event['type'] == 'workspace_response' and event['response']['kind'] == kind, event
    return event['response']['value']
def status(): return ws('status', 'status')
def change(action, **fields):
    review = ws('review', 'review', expected_revision=status()['revision'], change={'action': action, **fields})
    receipt = ws('apply', 'receipt', request=uid(), review=review['id'])
    assert not receipt['issues'], receipt
    return receipt['targets'][0]
def listing(**fields):
    query = dict(kind=None, project=None, home=None, repository=None, visibility='current', active_sessions_only=False)
    query.update(fields)
    rows, after = [], None
    while True:
        page = ws('list', 'page', query=query, after=after, limit=50)
        rows.extend(page['items']); after = page['next']
        if after is None: return rows
def launch(placement, cwd):
    event = rpc('primary_launch', request={'request': uid(), 'expected_revision': status()['revision'],
        'input': {'placement': placement, 'cwd': cwd, 'agent': None, 'model': None, 'selfdev': False}})
    assert event['type'] == 'primary_launch_response', event
    return event['response']
def launched(placement, cwd):
    response = launch(placement, {'kind': 'existing', 'path': str(cwd)})
    assert response['status'] == 'launched', response
    return response['record']['session']
def view(session):
    event = rpc('primary_location', command={'action': 'inspect_session', 'session': session})
    assert event['type'] == 'primary_location_response' and event['response']['status'] == 'session', event
    return event['response']['view']
def saved(session): return json.loads((f.home / 'sessions' / f'{session}.json').read_text())
def git(cwd, *args):
    subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', *args], cwd=cwd, check=True, capture_output=True)
def turn(session, text):
    before = len(posts)
    receipt = rpc('primary_input', input={'id': uid(), 'session': session, 'delivery': 'safe_boundary', 'content': text})
    assert receipt['type'] == 'primary_input_receipt', receipt
    wait(lambda: len(posts) > before, 'turn reached provider ' + text)
    return posts[-1]
try:
    f.start()
    step('organize: project, two repositories, directory reference, flat work area')
    ws('initialize', 'status', request=uid())
    volumes = ws('volumes', 'volumes')
    volume = next(v for v in volumes if v['writable'] and os.stat(v['mount']).st_dev == os.stat(ROOT).st_dev)
    project = change('create_project', name='Alpha')
    api = change('create_repository', name='api', remotes=[])
    web = change('create_repository', name='web', remotes=[])
    change('associate_repository', project=project['id'], repository=api['id'])
    change('associate_repository', project=project['id'], repository=web['id'])
    area = change('create_work_area', project=project['id'], name='spike')
    docs = ROOT / 'docs'; docs.mkdir()
    docs_loc = change('register_location', name='docs', path=str(docs), registration={'kind': 'directory', 'home': {'kind': 'project', 'id': project['id']}})
    notes = ROOT / 'notes'; notes.mkdir()
    notes_loc = change('register_location', name='notes', path=str(notes), registration={'kind': 'directory', 'home': {'kind': 'project', 'id': project['id']}})

    step('intentional independent clone into the work area')
    source = ROOT / 'source'; source.mkdir(); git(source, 'init', '-q', '-b', 'main')
    (source / 'README').write_text('fixture\n'); git(source, 'add', 'README'); git(source, 'commit', '-qm', 'base')
    destination = ROOT / 'clone'
    spec = dict(home={'kind': 'work_area', 'id': area['id']}, repository=api['id'], name='api-spike',
                base={'kind': 'branch', 'name': 'main'}, remotes=[], source={'kind': 'local', 'path': str(source)},
                branch={'kind': 'create', 'name': 'spike'},
                destination={'kind': 'custom', 'volume_uuid': volume['uuid'], 'path': str(destination)}, submodules=False, lfs=False)
    review = ws('review_clone', 'clone_review', expected_revision=status()['revision'], spec=spec)
    clone_request = uid()
    ws('begin_clone', 'clone', request=clone_request, review=review['id'])
    ready = wait(lambda: (lambda r: r if r['state'] in ('ready', 'preparation_failed', 'cancelled', 'recovery_required') else None)(ws('inspect_clone', 'clone', request=clone_request)), 'clone finished', 120)
    assert ready['state'] == 'ready', ready
    checkout = ready['location']
    checkouts_before = [row for row in listing(kind='location', project=project['id'], visibility='all')]

    step('two sessions share the clone')
    placement = {'kind': 'existing', 'placement': {'kind': 'checkout', 'id': checkout}}
    s1 = launched(placement, destination)
    s2 = launched(placement, destination)
    assert s1 != s2
    for session in (s1, s2):
        location = view(session)['location']
        assert location['placement'] == {'kind': 'checkout', 'id': checkout} and location['cwd'] == str(destination.resolve()), location
    checkouts_after = listing(kind='location', project=project['id'], visibility='all')
    assert len(checkouts_after) == len(checkouts_before), (checkouts_before, checkouts_after)
    assert sorted(p.name for p in ROOT.iterdir() if (p / '.git').exists()) == ['clone', 'source'], 'no second clone'
    facts = json.dumps(saved(s1)['messages'])
    for identity in (checkout, area['id'], project['id']):
        assert identity in facts, ('initial location facts', identity)

    step('project-level session needs an explicit cwd, then uses one')
    missing = launch({'kind': 'existing', 'placement': {'kind': 'project', 'id': project['id']}}, None)
    assert missing['status'] == 'rejected' and missing['issue']['code'] == 'needs_cwd', missing
    s3 = launched({'kind': 'existing', 'placement': {'kind': 'project', 'id': project['id']}}, docs)
    assert view(s3)['location']['placement'] == {'kind': 'project', 'id': project['id']}
    assert view(s3)['location']['cwd'] == str(docs.resolve())

    step('projectless standalone session')
    loose = ROOT / 'loose'; loose.mkdir()
    s4 = launched({'kind': 'standalone', 'root': str(loose)}, loose)
    standalone = view(s4)['location']['placement']
    assert standalone['kind'] == 'standalone', standalone
    standalone_entity = ws('inspect', 'entity', target={'kind': 'location', 'id': standalone['id']})
    assert standalone_entity['value']['home'] is None, standalone_entity

    step('archive keeps active work visible and usable; unarchive restores the rest')
    change('archive', target=project, archived=True)
    # Rows with active sessions (the clone, its area and the project, which
    # hosts the project-level session) stay visible; idle rows are hidden.
    current = {row['value']['id'] for row in listing(kind='location', project=project['id'])}
    assert current == {checkout}, current
    assert project['id'] in {row['value']['id'] for row in listing(kind='project')}
    assert area['id'] in {row['value']['id'] for row in listing(kind='work_area', project=project['id'])}
    sent = turn(s1, 'ARCHIVED BUT ACTIVE')
    assert 'ARCHIVED BUT ACTIVE' in sent
    assert view(s1)['location']['placement'] == {'kind': 'checkout', 'id': checkout}
    change('archive', target=project, archived=False)
    current = {row['value']['id'] for row in listing(kind='location', project=project['id'])}
    assert {checkout, docs_loc['id'], notes_loc['id']} <= current, current

    result.update(status='passed', checkout=checkout, sessions=[s1, s2, s3, s4], provider_posts=len(posts))
except Exception:
    result['error'] = traceback.format_exc()
finally:
    step('cleanup')
    cleanup = {}
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try: f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired: f.proc.kill(); f.proc.wait(timeout=10)
    cleanup['daemon_exit'] = f.proc.poll() if f.proc else None
    try: f.http.shutdown(); f.http.server_close(); f.log.close()
    except Exception: pass
    (evidence / 'steps.json').write_text(json.dumps(steps, indent=1))
    (evidence / 'result.json').write_text(json.dumps(result, indent=1))
    (evidence / 'cleanup.json').write_text(json.dumps(cleanup, indent=1))
    print(json.dumps(result, indent=1)); print('artifacts=' + str(ROOT))
if result['status'] != 'passed': raise SystemExit(1)
