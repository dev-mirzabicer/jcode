#!/usr/bin/env python3
"""Native acceptance of session placement once managed launch is rolled out
(SP-58-C01/WP-12).

With features.managed_primary_launch=true, an ordinary new session starts
unplaced. This journey drives the real daemon, the `jcode run` CLI and a real
TUI tester (PTY, physical keys, captured frames) through:

  0. no catalog yet: input refused before acceptance with the placement
     message (not a raw I/O error); review reports "not initialized"
  1. catalog initialized: capability and rollout flag are advertised
  2. first message to a fresh session is refused before it reaches history
     or the provider
  3. propose -> stale place refused -> place -> the message then runs once
  4. a second session in the same directory reuses the registered location
  5. a home-directory session gets a broad candidate with no default
  6. `jcode run` refuses without --place, runs once with --place
  7. TUI: Enter on a fresh session opens the review over the conversation;
     Enter places and sends the held message exactly once; in another session
     Esc keeps the message in the composer and sends nothing; frames at
     120x32, 80x24 and 48x12

All state is private and disposable. Inference is a localhost script; no paid
model is called. Run through scripts/run_isolated_test.py with
--binary <candidate> --artifact-dir <dir>.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, signal, subprocess, sys, time, traceback, uuid
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_instruction_manager as f

MARK = 'This session has no workspace placement yet'
config = f.home / 'config.toml'
config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))

posts = []
def provider(self):
    body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
    posts.append(json.dumps(body))
    chunks = [({'content': 'fixture reply'}, None), ({}, 'stop')]
    payload = ''.join('data: ' + json.dumps({'id': 'p', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': d, 'finish_reason': r}]}) + '\n\n' for d, r in chunks) + 'data: [DONE]\n\n'
    data = payload.encode()
    self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
f.FixtureProvider.do_POST = provider

ROOT = f.ROOT
evidence = ROOT / 'placement-evidence'; evidence.mkdir()
steps, frames, testers = [], [], []
result = {'status': 'failed'}

def step(name):
    steps.append({'step': name, 'at': time.time(), 'posts': len(posts)}); print('step:', name, flush=True)

def wait(predicate, label, seconds=60):
    deadline = time.monotonic() + seconds; last = None
    while time.monotonic() < deadline:
        try:
            last = predicate()
            if last: return last
        except (ValueError, KeyError, TypeError, IndexError) as error:
            last = error
        time.sleep(0.1)
    raise TimeoutError(f'{label}: {last!r}')

def git(cwd, *args):
    subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', *args], cwd=cwd, check=True, capture_output=True)

def repo(name):
    path = ROOT / name; path.mkdir(); git(path, 'init', '-q'); (path / 'src').mkdir()
    return path.resolve()

def rpc(kind, **fields):
    f.counter += 1; ident = f.counter
    f.send({'type': kind, 'id': ident, **fields})
    return f.until(lambda event: event.get('id') == ident and event.get('type') != 'ack')

def location(command):
    event = rpc('primary_location', command=command)
    assert event['type'] == 'primary_location_response', event
    with (evidence / 'location.jsonl').open('a') as out:
        out.write(json.dumps({'command': command, 'response': event['response']}) + '\n')
    return event['response']

def proposal_of(session):
    response = location({'action': 'propose_placement', 'session': session})
    assert response['status'] == 'proposal', response
    return response['proposal']

def connect():
    """A new client connection. One connection attaches one session, so
    every fresh session uses its own; earlier sessions stay runtime-owned."""
    import socket
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    f.client = socket.socket(socket.AF_UNIX); f.client.settimeout(30)
    f.client.connect(str(f.sockpath)); f.reader = f.client.makefile('rb')

def subscribe(cwd):
    connect()
    f.counter += 1; ident = f.counter
    f.send({'type': 'subscribe', 'id': ident, 'working_dir': str(cwd), 'selfdev': False, 'agent': 'global:jcode'})
    done = f.until(lambda e: e.get('id') == ident and e.get('type') in ('done', 'error'))
    assert done['type'] == 'done', done
    return [e['session_id'] for e in f.events if e.get('type') == 'session'][-1]

def message(text):
    f.counter += 1; ident = f.counter
    f.send({'type': 'message', 'id': ident, 'content': text, 'images': []})
    return f.until(lambda e: e.get('id') == ident and e.get('type') in ('done', 'error'))

def history(session):
    root = f.home / 'sessions'
    return ''.join(p.read_text(errors='replace') for p in root.glob(session + '*') if p.is_file())

def cli(*args, cwd, timeout=120):
    run = subprocess.run([f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', *args],
                         env=f.env, cwd=cwd, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    with (evidence / 'cli.jsonl').open('a') as out:
        out.write(json.dumps({'args': args, 'cwd': str(cwd), 'code': run.returncode, 'stdout': run.stdout[-3000:], 'stderr': run.stderr[-3000:]}) + '\n')
    return run

# --- TUI testers, spawned by a separate harness daemon that owns their PTYs.
HARNESS_SOCK = ROOT / 'h.sock'
harness = {'proc': None}

def start_harness():
    args = [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture',
            '--socket', str(HARNESS_SOCK), '-C', str(f.project), '--debug-socket', 'serve', '--server-name', 'wp12-harness']
    env = dict(f.env, JCODE_RUNTIME_DIR=str(ROOT / 'harness-runtime'), JCODE_SOCKET=str(HARNESS_SOCK))
    harness['env'] = env
    harness['proc'] = subprocess.Popen(args, env=env, stdout=open(ROOT / 'harness.log', 'wb'), stderr=subprocess.STDOUT)
    wait(lambda: HARNESS_SOCK.exists(), 'harness daemon socket', 60)

def harness_debug(command):
    out = subprocess.run([f.BIN, 'debug', command, '--socket', str(HARNESS_SOCK)], env=harness['env'], capture_output=True, text=True, timeout=25)
    assert out.returncode == 0, (command, out.stdout, out.stderr)
    return out.stdout

def tester_client(tid, text):
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
    assert 'ERR' not in tester_client(tid, 'keys:' + ','.join(specs))

def spawn(cwd, cols, rows):
    args = [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture', '--model', 'fixture', '--socket', str(f.sockpath)]
    wrapper = ROOT / f'client-{len(testers)}.sh'
    quote = lambda a: "'" + str(a).replace("'", "'\\''") + "'"
    wrapper.write_text('#!/bin/sh\nexport JCODE_RUNTIME_DIR=' + quote(f.env['JCODE_RUNTIME_DIR']) + ' JCODE_SOCKET=' + quote(f.sockpath) + '\nexec ' + ' '.join(map(quote, args)))
    wrapper.chmod(0o700)
    harness_debug('tester:spawn ' + json.dumps({'cwd': str(cwd), 'binary': str(wrapper), 'cols': cols, 'rows': rows}))
    tid = json.loads((f.home / 'testers.json').read_text())[-1]['id']; testers.append(tid)
    wait(lambda: json.loads(tester_client(tid, 'state')).get('server_version'), 'tester attached', 90)
    tester_client(tid, 'enable')
    keys(tid, 'esc', 'esc')
    return tid

def review(tid):
    return json.loads(tester_client(tid, 'placement-review-state'))

def frame(tid, name, expect):
    def capture():
        raw = tester_client(tid, 'screen-json')
        if not raw.lstrip().startswith('{'): return None
        value = json.loads(raw)
        if 'placement_review' not in value.get('render_order', []): return None
        text = value.get('rendered_text', {}).get('overlay_text') or ''
        return value if expect in text else None
    value = wait(capture, 'frame ' + name, 30)
    (evidence / (name + '.json')).write_text(json.dumps(value, indent=1))
    (evidence / (name + '.txt')).write_text(value['rendered_text']['overlay_text'])
    frames.append(name)
    return value['rendered_text']['overlay_text']

def client_pids():
    rows = subprocess.run(['ps', '-axo', 'pid=,command='], capture_output=True, text=True).stdout.splitlines()
    return [int(r.strip().partition(' ')[0]) for r in rows
            if ' serve' not in r.strip().partition(' ')[2] and r.strip().partition(' ')[2].startswith(str(f.BIN)) and str(f.sockpath) in r]

try:
    f.start()
    alpha = repo('alpha')
    step('0: no catalog yet, input refused before acceptance, review says not initialized')
    s0 = subscribe(alpha / 'src')
    refused = message('PROMPT BEFORE INIT')
    assert refused['type'] == 'error' and MARK in refused['message'], refused
    assert 'No such file' not in refused['message'], refused
    assert 'PROMPT BEFORE INIT' not in history(s0)
    proposal = location({'action': 'propose_placement', 'session': s0})
    assert proposal['status'] == 'rejected' and 'not initialized' in proposal['issue']['detail'], proposal
    before_init = cli('run', '--place', 'RUN BEFORE INIT', cwd=alpha)
    assert before_init.returncode != 0 and 'not initialized' in before_init.stderr, before_init.stderr
    assert not posts, 'no provider call before placement'

    step('1: initialize; capability and rollout flag advertised')
    init = rpc('workspace', request={'action': 'initialize', 'request': str(uuid.uuid4())})
    assert init['type'] == 'workspace_response', init
    caps = rpc('primary_control_probe')
    assert caps['session_placement_version'] == 1 and caps['location_enabled'] is True, caps
    probe = rpc('workspace_probe')
    assert probe.get('managed_rollout') is True, probe

    step('2: fresh session, first message refused before history and provider')
    s1 = subscribe(alpha / 'src')
    assert s1 != s0
    refused = message('FIRST PROMPT')
    assert refused['type'] == 'error' and MARK in refused['message'], refused
    assert 'FIRST PROMPT' not in history(s1) and not posts

    step('3: propose, stale place refused, place, then the message runs once')
    proposal = location({'action': 'propose_placement', 'session': s1})
    assert proposal['status'] == 'proposal', proposal
    p = proposal['proposal']
    assert p['working_dir'] == str(alpha / 'src') and p['default'] == 0, p
    candidate = p['candidates'][0]
    assert candidate['placement'] == {'kind': 'standalone', 'root': str(alpha)} and not candidate['broad'], candidate
    stale = location({'action': 'place', 'request': {'request': str(uuid.uuid4()), 'session': s1, 'working_dir': p['working_dir'], 'expected_catalog_revision': p['catalog_revision'] + 7, 'placement': candidate['placement']}})
    assert stale['status'] == 'rejected' and stale['issue']['code'] == 'conflict', stale
    request = {'request': str(uuid.uuid4()), 'session': s1, 'working_dir': p['working_dir'], 'expected_catalog_revision': p['catalog_revision'], 'placement': candidate['placement']}
    placed = location({'action': 'place', 'request': request})
    assert placed['status'] == 'state' and placed['record']['state'] == 'complete', placed
    replay = location({'action': 'place', 'request': request})
    assert replay['status'] == 'state' and replay['record']['operation'] == placed['record']['operation'], replay
    view = location({'action': 'inspect_session', 'session': s1})['view']
    assert view['location']['placement']['kind'] == 'standalone' and view['location']['cwd'] == str(alpha / 'src'), view
    ran = message('FIRST PROMPT')
    assert ran['type'] == 'done', ran
    assert len(posts) == 1 and 'FIRST PROMPT' in posts[0]
    assert history(s1).count('FIRST PROMPT') >= 1

    step('4: a second session in the same directory reuses the location')
    s2 = subscribe(alpha)
    assert s2 not in (s0, s1)
    p2 = proposal_of(s2)
    assert p2['candidates'][0]['placement']['kind'] == 'existing', p2
    assert p2['candidates'][0]['placement']['placement']['kind'] == 'standalone', p2
    assert p2['candidates'][0]['placement']['placement']['id'] == view['location']['placement']['id'], (p2, view)

    step('5: home is offered without a default')
    home = Path(f.env['HOME']).resolve()
    s3 = subscribe(home)
    p3 = proposal_of(s3)
    assert p3['default'] is None and p3['candidates'][0]['broad'] is True, p3

    step('6: jcode run refuses without --place and runs once with it')
    beta = repo('beta')
    count = len(posts)
    plain = cli('run', 'RUN PROMPT', cwd=beta)
    assert plain.returncode != 0 and MARK in plain.stderr and '--place' in plain.stderr, plain.stderr
    assert len(posts) == count
    placed_run = cli('run', '--place', 'RUN PROMPT', cwd=beta)
    assert placed_run.returncode == 0 and 'fixture reply' in placed_run.stdout, (placed_run.stdout, placed_run.stderr)
    assert 'Placed this session' in placed_run.stderr, placed_run.stderr
    assert len(posts) == count + 1
    broad_run = cli('run', '--place', 'RUN AT HOME', cwd=home)
    assert broad_run.returncode != 0 and 'No placement is proposed' in broad_run.stderr, broad_run.stderr
    assert len(posts) == count + 1

    step('7a: TUI Enter opens the review; Enter places and sends once')
    start_harness()
    gamma = repo('gamma')
    tid = spawn(gamma, 120, 32)
    tester_client(tid, 'set_input:TUI PROMPT'); keys(tid, 'enter')
    state = wait(lambda: (lambda s: s if s['visible'] and s['stage'] == 'Choosing' else None)(review(tid)), 'review open', 60)
    assert state['resend'] is True and state['composer'] == 'TUI PROMPT', state
    assert 'TUI PROMPT' not in state['user_display_messages'], state
    assert state['candidates'][0]['root'] == str(gamma) and state['default'] == 0, state
    text = frame(tid, 'review-120x32', 'Place this session')
    assert 'New standalone location gamma' in text and 'place and send' in text, text
    assert '/gamma' in text, text
    count = len(posts)
    keys(tid, 'enter')
    wait(lambda: not review(tid)['visible'], 'review closed after placement', 60)
    wait(lambda: len(posts) == count + 1, 'held message sent once', 60)
    wait(lambda: review(tid)['composer'] == '', 'composer emptied', 30)
    time.sleep(2)
    assert len(posts) == count + 1 and 'TUI PROMPT' in posts[-1]
    assert review(tid)['user_display_messages'].count('TUI PROMPT') == 1, review(tid)
    assert history(state['session']).count('TUI PROMPT') >= 1

    step('7b: Esc keeps the message and sends nothing; narrow frames')
    delta = repo('delta')
    tid2 = spawn(delta, 80, 24)
    tester_client(tid2, 'set_input:KEEP ME'); keys(tid2, 'enter')
    held = wait(lambda: (lambda s: s if s['visible'] and s['stage'] == 'Choosing' else None)(review(tid2)), 'second review open', 60)
    frame(tid2, 'review-80x24', 'Place this session')
    harness_debug('tester:' + tid2 + ':resize:48x12')
    time.sleep(1)
    frame(tid2, 'review-48x12', 'Place this')
    count = len(posts)
    keys(tid2, 'esc')
    wait(lambda: not review(tid2)['visible'], 'review closed by Esc', 30)
    state = review(tid2)
    assert state['composer'] == 'KEEP ME' and not state['resend_pending'], state
    assert 'KEEP ME' not in state['user_display_messages'], state
    time.sleep(2)
    assert len(posts) == count
    assert 'KEEP ME' not in history(held['session'])

    result.update(status='passed', sessions={'s0': s0, 's1': s1, 's2': s2, 's3': s3}, frames=frames, provider_posts=len(posts))
except Exception:
    result['error'] = traceback.format_exc()
finally:
    step('cleanup')
    cleanup = {}
    for tid in list(testers):
        try: cleanup[tid] = harness_debug('tester:' + tid + ':stop')
        except Exception as error: cleanup[tid] = repr(error)
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try: f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired: f.proc.kill(); f.proc.wait(timeout=10)
    cleanup['daemon_exit'] = f.proc.poll() if f.proc else None
    for pid in client_pids():
        cleanup.setdefault('terminated_clients', []).append(pid); os.kill(pid, signal.SIGTERM)
    if harness['proc'] and harness['proc'].poll() is None:
        harness['proc'].terminate()
        try: harness['proc'].wait(timeout=30)
        except subprocess.TimeoutExpired: harness['proc'].kill()
    cleanup['harness_exit'] = harness['proc'].poll() if harness['proc'] else None
    time.sleep(1)
    cleanup['leftover_clients'] = client_pids()
    try: f.http.shutdown(); f.http.server_close(); f.log.close()
    except Exception: pass
    (evidence / 'steps.json').write_text(json.dumps(steps, indent=1))
    (evidence / 'result.json').write_text(json.dumps(result, indent=1))
    (evidence / 'cleanup.json').write_text(json.dumps(cleanup, indent=1, default=str))
    print(json.dumps(result, indent=1)); print('artifacts=' + str(ROOT))
if result['status'] != 'passed': raise SystemExit(1)
