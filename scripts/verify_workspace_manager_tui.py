#!/usr/bin/env python3
"""Physical-input acceptance of the /workspace management mode (SP-58-C01/WP-10).

Drives a real TUI tester (PTY) attached to an owned fixture daemon with keys,
mouse clicks and PTY resizes. All state is private and disposable: catalog,
sessions, Git fixtures, clones and the daemon belong to this run. Inference is a
localhost script that is never asked to answer; no paid model is called.

Run through scripts/run_isolated_test.py with --binary <candidate>.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, signal, subprocess, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

config = f.home / 'config.toml'
config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))
gate_entered = threading.Event(); gate_release = threading.Event(); calls = []

def respond(self):
    self.rfile.read(int(self.headers.get('Content-Length', '0')))
    calls.append(time.time())
    gate_entered.set()
    gate_release.wait(300)
    self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
    value = {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {'content': 'held turn finished'}, 'finish_reason': 'stop'}]}
    try:
        self.wfile.write(('data: ' + json.dumps(value) + '\n\ndata: [DONE]\n\n').encode()); self.wfile.flush()
    except OSError:
        pass
f.FixtureProvider.do_POST = respond

ROOT = f.ROOT
evidence = ROOT / 'workspace-evidence'; evidence.mkdir()
testers = []; frames = []; steps = []; extra_daemons = []

def step(name):
    steps.append({'step': name, 'at': time.time()}); print('step:', name, flush=True)

def wait(predicate, label, seconds=60):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            last = predicate()
            if last: return last
        except (ValueError, KeyError, TypeError) as error:
            last = error
        time.sleep(0.1)
    raise TimeoutError(f'{label}: {last!r}')

def client(tid, text):
    """Write a command to the tester's own debug file, like the server does."""
    row = next(row for row in json.loads((f.home / 'testers.json').read_text()) if row['id'] == tid)
    target = Path(row['debug_cmd_path']); reply = Path(row['debug_response_path'])
    if reply.exists(): reply.unlink()
    temporary = target.with_suffix('.next'); temporary.write_text(text); os.replace(temporary, target)
    wait(lambda: reply.exists() and reply.stat().st_size > 0, 'tester reply to ' + text[:40], 20)
    value = reply.read_text(); reply.unlink()
    with (evidence / 'tester-commands.jsonl').open('a') as out:
        out.write(json.dumps({'at': time.time(), 'tester': tid, 'command': text, 'response': value[:4000]}) + '\n')
    return value

def keys(tid, *specs):
    """One debug command per batch: an idle tester reads commands at its
    deep-idle redraw cadence (about five seconds), so batching keeps the
    physical key sequence identical while bounding wall time."""
    for chunk in range(0, len(specs), 400):
        reply = client(tid, 'keys:' + ','.join(specs[chunk:chunk + 400]))
        assert 'ERR' not in reply, reply

def spec(char):
    if char == ' ': return 'space'
    if char == ',': raise ValueError('commas cannot be typed through key specs')
    if char.isupper(): return 'shift+' + char.lower()
    return char

def type_text(tid, text):
    keys(tid, *[spec(c) for c in text])

def manager(tid):
    return json.loads(client(tid, 'workspace-manager-state'))

def until(tid, predicate, label, seconds=60):
    return wait(lambda: (lambda s: s if predicate(s) else None)(manager(tid)), label, seconds)

def frame(tid, name, expect=None):
    def capture():
        raw = client(tid, 'screen-json')
        if not raw.lstrip().startswith('{'): return None
        value = json.loads(raw)
        text = value.get('rendered_text', {}).get('overlay_text') or ''
        if 'workspace_manager' not in value.get('render_order', []): return None
        if expect and expect not in text: return None
        return value
    value = wait(capture, 'frame ' + name, 20)
    (evidence / (name + '.json')).write_text(json.dumps(value, indent=1))
    (evidence / (name + '.txt')).write_text(value['rendered_text']['overlay_text'])
    frames.append(name)
    return value['rendered_text']['overlay_text']

def spawn(cols, rows, session=None):
    args = [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', '--socket', str(f.sockpath)]
    if session: args += ['--resume', session]
    wrapper = ROOT / f'client-{len(testers)}.sh'
    wrapper.write_text('#!/bin/sh\nexec ' + ' '.join(map(lambda a: "'" + a.replace("'", "'\\''") + "'", args)))
    wrapper.chmod(0o700)
    f.debug('tester:spawn ' + json.dumps({'cwd': str(f.project), 'binary': str(wrapper), 'cols': cols, 'rows': rows}))
    tid = json.loads((f.home / 'testers.json').read_text())[-1]['id']; testers.append(tid)
    wait(lambda: json.loads(client(tid, 'state')).get('server_version'), 'tester attached', 90)
    client(tid, 'enable')
    # The isolated home has no login; skip the onboarding welcome like a user.
    keys(tid, 'esc', 'esc')
    return tid

def open_manager(tid, command='/workspace'):
    for _ in range(5):
        if not manager(tid)['visible']: break
        keys(tid, 'esc')
    client(tid, 'set_input:' + command); keys(tid, 'enter')
    try:
        until(tid, lambda s: s['visible'], 'manager visible after ' + command, 15)
    except TimeoutError:
        (evidence / 'open-failure-state.json').write_text(client(tid, 'state'))
        (evidence / 'open-failure-screen.json').write_text(client(tid, 'screen-json'))
        raise

def rpc(kind, **fields):
    f.counter += 1; ident = f.counter
    f.send({'type': kind, 'id': ident, **fields})
    return f.until(lambda event: event.get('id') == ident and event.get('type') != 'ack')

def ws(action, **fields):
    event = rpc('workspace', request={'action': action, **fields}); assert event['type'] == 'workspace_response', event
    return event['response']

def git(cwd, *args):
    subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', *args], cwd=cwd, check=True, capture_output=True)

def select_row(tid, predicate, label):
    """Move the selection with physical arrow keys until a row matches."""
    for _ in range(20):
        state = manager(tid)
        ids = [row['key'] for row in state['rows']]
        target = next((row['key'] for row in state['rows'] if predicate(row['text'])), None)
        if target is None:
            time.sleep(0.5); continue
        if state['selected'] == target:
            return target
        current = ids.index(state['selected']) if state['selected'] in ids else 0
        delta = ids.index(target) - current
        keys(tid, *(['down'] * delta if delta > 0 else ['up'] * -delta))
    raise TimeoutError('row ' + label)

def focus_moves(form, key):
    order = form['visible']
    current = order.index(form['focused']) if form['focused'] in order else 0
    delta = order.index(key) - current
    return ['down'] * delta if delta > 0 else ['up'] * -delta

def set_text(tid, key, value):
    form = manager(tid)['form']
    keys(tid, *focus_moves(form, key), 'ctrl+u', *[spec(c) for c in value])
    form = manager(tid)['form']
    assert form['focused'] == key and form['values'][key] == value, (key, value, form)

def set_choice(tid, key, value):
    form = manager(tid)['form']
    choices = form['choices'][key]
    assert value in choices, (key, value, choices)
    rights = (choices.index(value) - choices.index(form['values'][key])) % len(choices)
    keys(tid, *focus_moves(form, key), *(['right'] * rights))
    form = manager(tid)['form']
    if form['values'][key] != value:
        # Choice lists do not wrap at their ends; walk back explicitly.
        keys(tid, *(['left'] * len(choices)), *(['right'] * choices.index(value)))
        form = manager(tid)['form']
    assert form['values'][key] == value, (key, value, form)

def set_toggle(tid, key, on):
    form = manager(tid)['form']
    moves = focus_moves(form, key)
    toggle = ['space'] if (form['values'][key] == 'true') != on else []
    keys(tid, *moves, *toggle)
    assert (manager(tid)['form']['values'][key] == 'true') == on

CONFLICTS = []

def submit_and_confirm(tid, title_part, typed=None):
    keys(tid, 'ctrl+s')
    state = until(tid, lambda s: s['confirm'] is not None or (s['form'] and s['form']['error']), 'review for ' + title_part)
    if not state['confirm'] and 'Conflict' in (state['form']['error'] or ''):
        # Another writer advanced the catalog. The draft came back intact and
        # the status refreshed; resubmitting reviews it against current state.
        CONFLICTS.append({'title': title_part, 'error': state['form']['error'], 'values': state['form']['values']})
        (evidence / 'conflicts.json').write_text(json.dumps(CONFLICTS, indent=1))
        time.sleep(1)
        keys(tid, 'ctrl+s')
        state = until(tid, lambda s: s['confirm'] is not None or (s['form'] and s['form']['error'] and 'Conflict' not in s['form']['error']), 'review after conflict ' + title_part)
    assert state['confirm'], state['form']
    assert title_part.lower() in state['confirm']['title'].lower(), state['confirm']
    return confirm(tid, typed)

MARK = []

def confirm(tid, typed=None):
    review = manager(tid)['confirm']
    MARK[:] = [manager(tid)['outcome_seq']]
    (evidence / 'reviews.jsonl').open('a').write(json.dumps(review) + '\n')
    if typed:
        type_text(tid, typed); keys(tid, 'tab'); keys(tid, 'enter')
    else:
        keys(tid, 'y')
    until(tid, lambda s: s['confirm'] is None, 'review closed')
    return review

def new_outcomes(state):
    outcomes = state['outcomes']
    if not MARK: return outcomes
    # By sequence, not text: identical outcome texts stay distinguishable.
    return outcomes[:max(0, state['outcome_seq'] - MARK[0])]

def last_outcome(tid, needle, label, seconds=90):
    """Wait for an outcome produced after the most recent confirmation."""
    return until(tid, lambda s: any(needle in text for text in new_outcomes(s)), label, seconds)

def daemon_pids():
    rows = subprocess.run(['ps', '-axo', 'pid=,command='], capture_output=True, text=True).stdout.splitlines()
    owned = []
    for row in rows:
        pid, _, command = row.strip().partition(' ')
        if ' serve' in command and str(f.sockpath) in command: owned.append(int(pid))
    return owned

def fixture_connect():
    import socket as socketlib
    f.client = socketlib.socket(socketlib.AF_UNIX); f.client.connect(str(f.sockpath))
    f.client.settimeout(120); f.reader = f.client.makefile('rb')

def cli(*args, timeout=180):
    run = subprocess.run([f.BIN, '--no-update', '--socket', str(f.sockpath), *args], env=f.env, capture_output=True, text=True, timeout=timeout)
    with (evidence / 'cli.jsonl').open('a') as out:
        out.write(json.dumps({'args': args, 'code': run.returncode, 'stdout': run.stdout[-4000:], 'stderr': run.stderr[-4000:]}) + '\n')
    return run


result = {'status': 'failed', 'binary': f.BIN, 'root': str(ROOT)}
try:
    # Disposable Git source and bare remote, owned by this run.
    source = ROOT / 'source'; source.mkdir(); git(source, 'init', '-b', 'main')
    (source / 'README.md').write_text('fixture\n'); git(source, 'add', '.'); git(source, 'commit', '-m', 'fixture')
    remote = ROOT / 'remote.git'; git(ROOT, 'clone', '--bare', str(source), str(remote))
    docs = ROOT / 'docs'; docs.mkdir()
    (ROOT / 'checkouts').mkdir(); (ROOT / 'volume-default').mkdir()
    query = lambda kind=None: {'kind': kind, 'project': None, 'home': None, 'repository': None, 'visibility': 'all', 'active_sessions_only': False}

    f.start(); f.client.settimeout(120)
    step('attach tester and open /workspace on an uninitialized catalog')
    legacy_session = f.subscribe()
    tid = spawn(120, 32, legacy_session)
    open_manager(tid)
    until(tid, lambda s: s['catalog'] and s['catalog'].get('detail') == 'Workspace is not initialized', 'uninitialized catalog')
    frame(tid, 'w01-uninitialized', 'not initialized')
    keys(tid, 'shift+i')
    until(tid, lambda s: s['confirm'] and 'Initialize' in s['confirm']['title'], 'initialize review')
    frame(tid, 'w02-initialize-review', 'Initialize the workspace catalog')
    keys(tid, 'n'); time.sleep(0.5)
    assert ws('status')['kind'] == 'error', 'declining the review must not initialize'
    keys(tid, 'shift+i'); confirm(tid)
    until(tid, lambda s: s['catalog'] and 'revision' in s['catalog'], 'catalog ready')

    step('organization: project, repository, work area, association, directory')
    keys(tid, 'n'); until(tid, lambda s: s['form'], 'project form')
    type_text(tid, 'Acme'); submit_and_confirm(tid, 'organization'); last_outcome(tid, 'Done', 'project created')
    keys(tid, 'shift+n'); until(tid, lambda s: s['form'], 'repository form')
    set_text(tid, 'name', 'Tool'); set_text(tid, 'remotes', 'file://' + str(remote))
    submit_and_confirm(tid, 'organization'); last_outcome(tid, 'Done', 'repository created')
    select_row(tid, lambda t: t.startswith('P  Acme'), 'Acme')
    keys(tid, 'a'); until(tid, lambda s: s['form'] and s['form']['title'] == 'New work area', 'area form')
    set_text(tid, 'name', 'Sprint'); submit_and_confirm(tid, 'organization'); last_outcome(tid, 'Done', 'area created')
    entities = {e['value']['name']: e['value'] for e in ws('list', query=query(), after=None, limit=200)['value']['items']}
    acme = entities['Acme']['id']; tool = entities['Tool']['id']; sprint = entities['Sprint']['id']
    keys(tid, 'shift+a'); until(tid, lambda s: s['form'] and 'Associate' in s['form']['title'], 'associate form')
    set_choice(tid, 'project', 'project:' + acme); set_choice(tid, 'repository', 'repository:' + tool)
    submit_and_confirm(tid, 'organization'); last_outcome(tid, 'Done', 'associated')
    keys(tid, 'g'); until(tid, lambda s: s['form'] and 'Register' in s['form']['title'], 'register form')
    set_choice(tid, 'kind', 'directory'); set_text(tid, 'path', str(docs)); set_text(tid, 'name', 'Docs')
    set_choice(tid, 'home', 'project:' + acme)
    submit_and_confirm(tid, 'organization'); last_outcome(tid, 'Done', 'directory registered')
    frame(tid, 'w03-organization-wide', 'Acme')

    step('volumes: saved default, then custom and default clone destinations')
    mount = subprocess.run(['df', '-P', str(ROOT)], capture_output=True, text=True).stdout.splitlines()[-1].split()[-1]
    data_volume = next(v for v in ws('volumes')['value'] if v['mount'] == mount)
    keys(tid, 'v'); until(tid, lambda s: s['form'] and 'Default checkout' in s['form']['title'], 'volume default form')
    set_choice(tid, 'volume', data_volume['uuid']); set_text(tid, 'path', str(ROOT / 'volume-default'))
    submit_and_confirm(tid, 'organization'); last_outcome(tid, 'Done', 'volume default')
    clones = {}
    for name, destination in (('tool-custom', 'custom'), ('tool-default', 'default')):
        select_row(tid, lambda t: t.startswith('R  Tool'), 'Tool repository')
        keys(tid, 'c'); until(tid, lambda s: s['form'] and 'Clone' in s['form']['title'], 'clone form')
        set_text(tid, 'name', name); set_choice(tid, 'home', 'area:' + sprint)
        set_choice(tid, 'source', 'local'); set_text(tid, 'local', str(source))
        set_text(tid, 'base', 'main'); set_text(tid, 'remotes', 'origin=file://' + str(remote))
        set_choice(tid, 'volume', data_volume['uuid']); set_choice(tid, 'destination', destination)
        if destination == 'custom': set_text(tid, 'custom', str(ROOT / 'checkouts' / name))
        else: set_text(tid, 'project_component', 'acme')
        review = submit_and_confirm(tid, 'Begin clone')
        expected = ROOT / 'checkouts' / name if destination == 'custom' else ROOT / 'volume-default' / 'acme' / name
        assert any(str(expected) in line for line in review['lines']), review
        clones[name] = expected
        last_outcome(tid, f'Clone {name} is Ready', 'clone ready ' + name, 240)
        assert (expected / '.git').is_dir() and (expected / 'README.md').read_text() == 'fixture\n'
        assert not (expected / '.git' / 'objects' / 'info' / 'alternates').exists()
    keys(tid, '3'); until(tid, lambda s: s['section'] == 'operations', 'operations section')
    keys(tid, 'u'); until(tid, lambda s: sum('Clone' in r['text'] for r in s['rows']) >= 2, 'finished clones listed')
    frame(tid, 'w04-operations', 'Ready')
    keys(tid, '1')
    locations = {e['value']['name']: e['value'] for e in ws('list', query=query('location'), after=None, limit=200)['value']['items']}

    step('launch a managed primary into the custom clone')
    select_row(tid, lambda t: t.startswith('C  tool-custom'), 'tool-custom')
    keys(tid, 'l'); until(tid, lambda s: s['form'] and 'Launch' in s['form']['title'], 'launch form')
    form = manager(tid)['form']
    assert form['values']['placement'] == 'checkout:' + locations['tool-custom']['id'], form
    assert form['values']['cwd'] == str(clones['tool-custom']), form
    submit_and_confirm(tid, 'Launch primary')
    launched = until(tid, lambda s: s['launched'], 'launched', 120)['launched']
    view = rpc('primary_location', command={'action': 'inspect_session', 'session': launched})['response']['view']
    assert view['location']['placement'] == {'kind': 'checkout', 'id': locations['tool-custom']['id']}, view

    step('move the launched session to the work area; cwd kept by default')
    keys(tid, '2'); until(tid, lambda s: s['section'] == 'sessions', 'sessions section')
    keys(tid, 'i'); until(tid, lambda s: s['form'], 'inspect form'); type_text(tid, launched); keys(tid, 'ctrl+s')
    until(tid, lambda s: s['selected'] == launched and any(a.startswith('m ') for a in s['actions']), 'launched session inspected')
    keys(tid, 'm'); until(tid, lambda s: s['form'] and 'Move' in s['form']['title'], 'move form')
    set_choice(tid, 'placement', 'area:' + sprint)
    review = submit_and_confirm(tid, 'Change session location')
    assert any(str(clones['tool-custom']) in line for line in review['lines'] if 'Working directory' in line), review
    last_outcome(tid, 'Complete', 'move complete')
    view = rpc('primary_location', command={'action': 'inspect_session', 'session': launched})['response']['view']
    assert view['location']['placement'] == {'kind': 'work_area', 'id': sprint} and view['location']['cwd'] == str(clones['tool-custom']), view
    frame(tid, 'w05-sessions-moved', 'Placement')

    step("adopt the tester's legacy session into the Docs directory")
    select_row(tid, lambda t: t.startswith('● '), 'own session')
    until(tid, lambda s: any(a.startswith('a Adopt') for a in s['actions']), 'legacy adoption offered')
    frame(tid, 'w06-legacy', 'Legacy session')
    keys(tid, 'a'); until(tid, lambda s: s['form'] and 'legacy' in s['form']['title'], 'adoption form')
    set_choice(tid, 'placement', 'directory:' + locations['Docs']['id']); set_text(tid, 'cwd', str(docs))
    submit_and_confirm(tid, 'Adopt legacy'); last_outcome(tid, 'Complete', 'adopted')
    view = rpc('primary_location', command={'action': 'inspect_session', 'session': legacy_session})['response']['view']
    assert view['location']['placement'] == {'kind': 'directory', 'id': locations['Docs']['id']}, view

    step('agent-side access proposal approved, then revoked, through the human client')
    proposal = ws('permissions', request={'action': 'propose', 'session': launched, 'request': str(uuid.uuid4()), 'target': {'kind': 'root', 'id': locations['Docs']['id']}, 'reason': 'needs docs'})
    assert proposal['kind'] == 'permissions', proposal
    keys(tid, '4'); until(tid, lambda s: s['section'] == 'permissions' and any('wants' in r['text'] for r in s['rows']), 'proposal listed')
    select_row(tid, lambda t: 'wants' in t and launched in t, 'proposal')
    keys(tid, 'a'); until(tid, lambda s: s['form'] and 'Approve' in s['form']['title'], 'approve form')
    assert manager(tid)['form']['values']['session'] == launched
    review = submit_and_confirm(tid, 'permission')
    assert any('writable now' in line and str(docs) in line for line in review['lines']), review
    last_outcome(tid, 'is Active', 'grant active')
    frame(tid, 'w07-permissions', 'Approved')
    keys(tid, 'v'); until(tid, lambda s: any(r['text'].startswith('Active') for r in s['rows']), 'grants listed')
    select_row(tid, lambda t: t.startswith('Active') and launched in t, 'active grant')
    keys(tid, 'x'); until(tid, lambda s: s['confirm'], 'revoke review'); confirm(tid)
    last_outcome(tid, 'is Revoked', 'grant revoked')

    step('direct grant to this session, then Clear with a reviewed carry choice')
    keys(tid, 'n'); until(tid, lambda s: s['form'] and 'grant' in s['form']['title'].lower(), 'grant form')
    set_text(tid, 'session', legacy_session); set_choice(tid, 'target', 'checkout:' + locations['tool-default']['id'])
    submit_and_confirm(tid, 'permission'); last_outcome(tid, 'is Active', 'direct grant')
    keys(tid, '2'); select_row(tid, lambda t: t.startswith('● '), 'own session')
    keys(tid, 'n'); until(tid, lambda s: s['form'] and 'New context' in s['form']['title'], 'new context form')
    set_choice(tid, 'kind', 'clear'); keys(tid, 'ctrl+s')
    until(tid, lambda s: s['form'] and s['form']['title'] == 'Grant carry', 'carry review')
    frame(tid, 'w08-carry', 'Grant carry')
    set_choice(tid, 'carry', 'yes'); keys(tid, 'ctrl+s')
    def copied():
        items = ws('permissions', request={'action': 'list', 'query': {'kind': 'grants', 'audience': None}, 'after': None, 'limit': 200})['value']['value']['items']
        return [g['value'] for g in items if g['value'].get('copied_from')]
    carried = wait(copied, 'carried grant copy', 60)
    assert carried[0]['state'] == 'active' and carried[0]['audience']['id'] != legacy_session, carried

    step('closeout with final human approval on tool-default')
    open_manager(tid); keys(tid, '1')
    select_row(tid, lambda t: t.startswith('C  tool-default'), 'tool-default')
    keys(tid, 'o'); until(tid, lambda s: s['form'] and 'closeout' in s['form']['title'].lower(), 'closeout form')
    assert manager(tid)['form']['values']['conditional'] == 'false'
    # Every regular file needs a disposition or the verified full archive.
    set_toggle(tid, 'full_archive', True)
    review = submit_and_confirm(tid, 'Begin closeout')
    assert any('off' in line for line in review['lines'] if 'Conditional' in line), review
    until(tid, lambda s: s['section'] == 'closeout' and s['selected'], 'closeout selected')
    def act(key, label, seconds=240):
        until(tid, lambda s: any(a.startswith(key + ' ') for a in s['actions']), 'action ' + label, seconds)
        keys(tid, key if len(key) == 1 and key.islower() else 'shift+' + key.lower())
    def closeout_step(key, label):
        act(key, label); until(tid, lambda s: s['confirm'], label + ' review'); confirm(tid)
        until(tid, lambda s: new_outcomes(s) and not s['pending'], label + ' outcome', 240)
        (evidence / ('closeout-' + key + '.json')).write_text(json.dumps(manager(tid), indent=1))
    closeout_step('f', 'Refresh inventory')
    # A human disposition on one inventoried entry (the full archive covers the rest).
    act('i', 'Inventory'); act('d', 'Disposition')
    until(tid, lambda s: s['form'] and s['form']['title'] == 'Record disposition', 'disposition form')
    set_choice(tid, 'kind', 'redundant'); set_text(tid, 'reason', 'Fixture data')
    MARK[:] = [manager(tid)['outcome_seq']]; keys(tid, 'ctrl+s')
    until(tid, lambda s: not s['form'] and new_outcomes(s) and not s['pending'], 'disposition recorded', 120)
    (evidence / 'closeout-d.json').write_text(json.dumps(manager(tid), indent=1))
    frame(tid, 'w09-closeout-inventory', 'Inventory')
    act('b', 'Record pane')
    closeout_step('p', 'Preserve')
    closeout_step('w', 'Review removal')
    frame(tid, 'w09-closeout-review', 'Removal review')
    act('a', 'Approve removal')
    until(tid, lambda s: s['confirm'] and s['confirm']['typed'] == 'approve', 'typed approval')
    confirm(tid, 'approve')
    act('F', 'Finish removal'); until(tid, lambda s: s['confirm'] and s['confirm']['typed'] == 'remove', 'typed removal')
    confirm(tid, 'remove')
    last_outcome(tid, 'Closed', 'closed', 240)
    assert not clones['tool-default'].exists(), 'disposable fixture checkout removed'
    keys(tid, 'shift+h'); frame(tid, 'w10-closed-history', 'History of')
    keys(tid, '1')
    def cycle_visibility(predicate, label):
        for _ in range(6):
            keys(tid, 'h'); time.sleep(0.4)
            state = manager(tid)
            if not state['pending'] and predicate(state): return state
        raise TimeoutError(label)
    cycle_visibility(lambda s: any(r['text'].startswith('C  tool-default') and 'Closed' in r['text'] for r in s['rows']), 'closed checkout under a visibility filter')
    frame(tid, 'w11-closed-filter', 'tool-default')
    cycle_visibility(lambda s: any(r['text'].startswith('C  tool-custom') for r in s['rows']) and not any(r['text'].startswith('C  tool-default') for r in s['rows']), 'back to current visibility')

    step('conditional no-loss authorization issued, then revoked, on tool-custom')
    select_row(tid, lambda t: t.startswith('C  tool-custom'), 'tool-custom')
    keys(tid, 'o'); until(tid, lambda s: s['form'], 'closeout form 2')
    set_toggle(tid, 'conditional', True)
    review = submit_and_confirm(tid, 'Begin closeout')
    assert any('ON' in line for line in review['lines']), review
    record = next(op for op in ws('operations', query={'target': None, 'session': None, 'kinds': ['closeout'], 'unfinished_only': True}, after=None, limit=50)['value']['items'])
    assert record['operation']['record']['spec']['conditional_no_loss'] is True, record
    act('X', 'Revoke closeout'); until(tid, lambda s: s['confirm'], 'revoke closeout review'); confirm(tid)
    last_outcome(tid, 'revoked', 'conditional closeout revoked')
    assert clones['tool-custom'].exists()

    step('backup, export, import and restore')
    keys(tid, '6'); until(tid, lambda s: s['section'] == 'backup', 'backup section')
    keys(tid, 'b'); until(tid, lambda s: s['form'], 'backup form'); set_text(tid, 'name', 'before-import')
    submit_and_confirm(tid, 'Back up'); last_outcome(tid, 'written to', 'backup')
    keys(tid, 'e'); until(tid, lambda s: s['form'], 'export form'); set_choice(tid, 'project', 'project:' + acme); set_text(tid, 'name', 'acme-export')
    submit_and_confirm(tid, 'Export'); state = last_outcome(tid, 'Exported project definition', 'exported')
    exported = next(t for t in new_outcomes(state) if 'Exported' in t).split(' to ', 1)[1].split('. It contains')[0]
    keys(tid, 'i'); until(tid, lambda s: s['form'], 'import form')
    set_text(tid, 'path', exported); set_choice(tid, 'collisions', 'new')
    review = submit_and_confirm(tid, 'Apply import')
    assert any('stay disabled' in line for line in review['lines']), review
    last_outcome(tid, 'apply import', 'imported')
    select_row(tid, lambda t: 'before-import' in t, 'named snapshot')
    keys(tid, 'shift+r'); until(tid, lambda s: s['confirm'] and s['confirm']['typed'] == 'restore', 'restore review')
    confirm(tid, 'restore'); last_outcome(tid, 'restore catalog snapshot', 'restored')
    frame(tid, 'w12-backup', 'before-import')

    step('layouts through real PTY resizes; mouse tabs and footer actions')
    for cols, rows, expect in ((80, 24, 'Workspace'), (60, 24, 'Workspace'), (48, 12, 'Workspace'), (40, 10, 'needs 48×12'), (120, 32, 'Workspace')):
        f.debug(f'tester:{tid}:resize:{cols}x{rows}')
        wait(lambda: manager(tid)['dimensions'] == [cols, rows], f'resize {cols}x{rows}', 20)
        frame(tid, f'w13-layout-{cols}x{rows}', expect)
    text = frame(tid, 'w14-before-click', '7 Runtime')
    lines = text.splitlines()
    row = next(i for i, line in enumerate(lines) if '7 Runtime' in line)
    client(tid, f'mouse:click:{lines[row].index("2 Sessions") + 1},{row}')
    until(tid, lambda s: s['section'] == 'sessions', 'mouse tab click')
    lines = frame(tid, 'w15-after-click', 'i Inspect by ID').splitlines()
    row = next(i for i, line in enumerate(lines) if 'i Inspect by ID' in line)
    client(tid, f'mouse:click:{lines[row].index("i Inspect by ID") + 1},{row}')
    until(tid, lambda s: s['form'] and 'Inspect' in s['form']['title'], 'mouse footer action')
    type_text(tid, 'Draft')

    step('a draft survives a runtime crash and explicit Start')
    for pid in daemon_pids(): os.kill(pid, signal.SIGKILL)
    if f.proc: f.proc.wait(timeout=20)
    until(tid, lambda s: not s['connected'], 'disconnected', 30)
    assert manager(tid)['form']['values']['session'] == 'Draft'
    started = cli('runtime', 'start', '--json'); assert started.returncode == 0, started.stderr
    until(tid, lambda s: s['connected'] and s['capabilities']['management'], 'reconnected', 120)
    state = manager(tid)
    assert state['form'] and state['form']['values']['session'] == 'Draft', state['form']
    frame(tid, 'w16-draft-after-reconnect', 'Draft')
    keys(tid, 'esc', 'esc'); until(tid, lambda s: not s['form'], 'draft discarded')

    step('runtime: finish-current stop waits for a held turn and is cancelled while waiting')
    fixture_connect(); gate_entered.clear(); gate_release.clear()
    f.subscribe(launched); f.send({'type': 'message', 'id': 9001, 'content': 'hold this turn'})
    assert gate_entered.wait(60), 'turn reached the fixture provider'
    open_manager(tid, '/runtime')
    until(tid, lambda s: s['section'] == 'runtime' and s['runtime']['desired_stopped'] is False, 'runtime status')
    keys(tid, 's'); until(tid, lambda s: s['form'] and 'Stop' in s['form']['title'], 'stop form')
    set_choice(tid, 'independent', 'keep')
    review = submit_and_confirm(tid, 'Stop the runtime')
    assert any('KeepSupported' in line for line in review['lines']) and any('PrimaryTurn' in line for line in review['lines']), review
    until(tid, lambda s: s['runtime']['operation'] and s['runtime']['operation']['phase'] == 'waiting_for_current', 'waiting for current', 60)
    frame(tid, 'w17-runtime-waiting', 'WaitingForCurrent')
    keys(tid, 'c'); until(tid, lambda s: s['confirm'], 'cancel wait review'); confirm(tid)
    until(tid, lambda s: s['runtime']['operation'] and s['runtime']['operation']['phase'] == 'cancelled', 'cancelled', 60)
    gate_release.set()

    step('runtime: reviewed stop, offline durable intent and Start from the TUI')
    time.sleep(2)
    keys(tid, 's'); until(tid, lambda s: s['form'] and 'Stop' in s['form']['title'], 'stop form 2')
    submit_and_confirm(tid, 'Stop the runtime')
    until(tid, lambda s: not s['connected'], 'runtime stopped; client offline', 120)
    until(tid, lambda s: s['runtime']['offline'] and 'S Start runtime' in s['actions'], 'offline start offered', 60)
    frame(tid, 'w18-runtime-stopped', 'intentionally stopped')
    assert not daemon_pids(), 'intentional stop is not undone by the client'
    keys(tid, 'shift+s')
    until(tid, lambda s: s['connected'] and s['capabilities']['runtime'], 'reconnected after TUI Start', 180)

    step('crash during a turn: recovery selection Leave stopped')
    fixture_connect(); gate_entered.clear(); gate_release.clear()
    f.subscribe(launched); f.send({'type': 'message', 'id': 9002, 'content': 'hold this turn again'})
    assert gate_entered.wait(60), 'second turn reached the fixture provider'
    for pid in daemon_pids(): os.kill(pid, signal.SIGKILL)
    gate_release.set()
    until(tid, lambda s: not s['connected'], 'crash observed', 30)
    started = cli('runtime', 'start', '--json'); assert started.returncode == 0, started.stderr
    until(tid, lambda s: s['connected'] and s['runtime']['recoveries'], 'recovery item visible', 180)
    open_manager(tid, '/runtime')
    select_row(tid, lambda t: t.startswith('Needs decision') and launched in t, 'recovery item')
    frame(tid, 'w19-recovery', 'Needs decision')
    keys(tid, 'shift+l'); until(tid, lambda s: s['confirm'], 'leave review'); confirm(tid)
    last_outcome(tid, 'left stopped', 'recovery resolved')
    result.update(status='passed', conflicts_recovered=len(CONFLICTS), launched=launched, legacy_session=legacy_session, clones={k: str(v) for k, v in clones.items()}, frames=frames)
except Exception:
    result['error'] = traceback.format_exc()
finally:
    step('cleanup')
    gate_release.set()
    cleanup = {}
    for tid in list(testers):
        try: cleanup[tid] = f.debug('tester:' + tid + ':stop')
        except Exception as error: cleanup[tid] = repr(error)
    try:
        status = cli('runtime', 'status', '--json', timeout=60)
        cleanup['final_status'] = status.stdout[-2000:]
        if json.loads(status.stdout).get('live_response'):
            review = json.loads(cli('runtime', 'stop', '--strategy', 'interrupt', '--json', timeout=60).stdout)
            cleanup['stop_review'] = review
            request = review.get('confirm_request')
            if request:
                cleanup['stop'] = subprocess.run(request if isinstance(request, list) else ['sh', '-c', request], env=f.env, capture_output=True, text=True, timeout=120).returncode
    except Exception as error:
        cleanup['stop_error'] = repr(error)
    for pid in daemon_pids():
        cleanup.setdefault('killed_leftover', []).append(pid); os.kill(pid, signal.SIGKILL)
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try: f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired: f.proc.kill()
    cleanup['leftover_daemons'] = daemon_pids()
    try: f.http.shutdown(); f.http.server_close(); f.log.close()
    except Exception: pass
    (evidence / 'steps.json').write_text(json.dumps(steps, indent=1))
    (evidence / 'result.json').write_text(json.dumps(result, indent=1))
    (evidence / 'cleanup.json').write_text(json.dumps(cleanup, indent=1, default=str))
    print(json.dumps(result, indent=1))
if result['status'] != 'passed': raise SystemExit(1)
