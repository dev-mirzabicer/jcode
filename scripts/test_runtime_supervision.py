#!/usr/bin/env python3
"""Runtime supervision journeys against owned fixtures. No paid model calls.

Planned reload/restart continuation, failed reload, crash and external-signal
recovery selection, automatic-wake deferral, and (macOS) an isolated namespaced
LaunchAgent with power-assertion observation. Every daemon, launchd job and
file belongs to this fixture; nothing touches the real runtime or its service.
Run via scripts/run_isolated_test.py with --binary and --artifact-dir.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Use scripts/run_isolated_test.py')
import hashlib, json, signal, socket, subprocess, sys, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc = Path(tempfile.mkdtemp(prefix='rs-', dir=os.environ['JCODE_RUNTIME_DIR']))
old = str(f.sockpath); f.sockpath = ipc / 'runtime.sock'
f.args = [str(f.sockpath) if arg == old else arg for arg in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
agents_dir = f.ROOT / 'LaunchAgents'; agents_dir.mkdir()
f.env['JCODE_LAUNCH_AGENTS_DIR'] = str(agents_dir)
# Any runtime started without explicit provider arguments (the login service)
# still resolves the local fixture, never a real credential.
# Credentials live in a file, as real ones do; the login service never
# receives the installer's environment credentials.
(f.home / 'config' / 'jcode').mkdir(parents=True, exist_ok=True)
(f.home / 'config' / 'jcode' / 'fixture.env').write_text('WP09_FIXTURE_KEY=synthetic-local-key\n')
config = (f.home / 'config.toml').read_text().replace(
    'api_key_env="WP09_FIXTURE_KEY"', 'api_key_env="WP09_FIXTURE_KEY"\nenv_file="fixture.env"', 1)
(f.home / 'config.toml').write_text(config + '\n[provider]\ndefault_provider="wp09-fixture"\ndefault_model="fixture"\n')
prefix = f.args[:f.args.index('serve')]
commands = []; clients = []; errors = []; cleanup = []; outcomes = {}
daemons = []; label = None
uid = os.getuid()

def progress(stage):
    (f.ROOT / 'progress.json').write_text(json.dumps({'stage': stage, 'time': time.time()}))
    print(json.dumps({'stage': stage, 'root': str(f.ROOT)}), flush=True)

def deadline(_signal, _frame):
    raise TimeoutError('Owned fixture watchdog expired')
signal.signal(signal.SIGALRM, deadline)
signal.alarm(900)

def text_of(message):
    content = message.get('content')
    if isinstance(content, list):
        return '\n'.join(part.get('text', '') for part in content if isinstance(part, dict))
    return content or ''

def provider(self):
    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
    messages = body.get('messages', [])
    # Everything the model has not answered yet: the user/context messages
    # after the last assistant reply (shell records may follow the prompt).
    pending = []
    for message in reversed(messages):
        if message.get('role') == 'assistant':
            break
        if message.get('role') == 'user':
            pending.insert(0, text_of(message))
    last = '\n'.join(pending)
    f.posts.append({'last': last, 'body': json.dumps(body)})
    (f.ROOT / 'posts.json').write_text(json.dumps(f.posts, indent=2))
    self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
    if 'HOLD:' in last:
        tag = last.rsplit('HOLD:', 1)[1].split()[0]
        limit = time.monotonic() + 120
        while not (f.ROOT / ('release-' + tag)).exists() and time.monotonic() < limit:
            time.sleep(0.05)
    try:
        for delta, reason in [({'content': 'FIXTURE_REPLY'}, None), ({}, 'stop')]:
            self.wfile.write(('data: ' + json.dumps({'id': 'fixture', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': reason}]}) + '\n\n').encode())
        self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
    except OSError:
        pass  # The interrupted runtime already closed this stream.
f.FixtureProvider.do_POST = provider

def wait(predicate, label_, seconds=40):
    limit = time.monotonic() + seconds
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() > limit:
            raise TimeoutError(label_)
        time.sleep(0.1)

def run(*args, success=True, timeout=90):
    value = subprocess.run(prefix + list(args), env=f.env, cwd=f.project, capture_output=True, text=True, timeout=timeout)
    commands.append({'args': list(args), 'exit': value.returncode, 'stdout': value.stdout[-4000:], 'stderr': value.stderr[-4000:]})
    (f.ROOT / 'cli.json').write_text(json.dumps(commands, indent=2))
    if success:
        assert value.returncode == 0, commands[-1]
    return value

def cli(*args, success=True):
    value = run('runtime', *args, '--json', success=success)
    return json.loads(value.stdout) if success else value

def status():
    return cli('status')

def start_daemon():
    proc = subprocess.Popen(f.args, env=f.env, stdout=f.log, stderr=f.log)
    daemons.append(proc)
    wait(lambda: proc.poll() is not None or (f.sockpath.exists() and connectable()), 'daemon ready', 40)
    assert proc.poll() is None, 'daemon exited: ' + (f.ROOT / 'server.log').read_text(errors='replace')[-3000:]
    return proc

def connectable():
    try:
        probe = socket.socket(socket.AF_UNIX); probe.settimeout(2); probe.connect(str(f.sockpath)); probe.close(); return True
    except OSError:
        return False

class Client:
    def __init__(self):
        self.sock = socket.socket(socket.AF_UNIX); self.sock.settimeout(45); self.sock.connect(str(f.sockpath))
        self.stream = self.sock.makefile('rwb', buffering=0); self.n = 0; self.events = []; clients.append(self)
    def send(self, kind, **fields):
        self.n += 1; self.stream.write((json.dumps({'type': kind, 'id': self.n, **fields}) + '\n').encode()); return self.n
    def until(self, predicate):
        while True:
            data = self.stream.readline()
            if not data:
                raise EOFError('owned runtime connection ended')
            event = json.loads(data); self.events.append(event)
            if event.get('type') == 'error':
                raise AssertionError(event)
            if predicate(event):
                return event
    def rpc(self, kind, **fields):
        ident = self.send(kind, **fields); return self.until(lambda e: e.get('id') == ident and e['type'] != 'ack')
    def subscribe(self, session=None):
        ident = self.send('subscribe', working_dir=str(f.project), selfdev=False, **({'target_session_id': session} if session else {'agent': 'global:jcode'}))
        self.until(lambda e: e.get('id') == ident and e['type'] == 'done')
        return [e['session_id'] for e in self.events if e['type'] in ('session', 'history')][-1]
    def history(self, session):
        ident = self.send('subscribe', working_dir=str(f.project), selfdev=False, target_session_id=session)
        self.until(lambda e: e.get('id') == ident and e['type'] == 'done')
        return [e for e in self.events if e['type'] == 'history'][-1]
    def execution(self, action, run, **fields):
        event = self.rpc('execution', request={'action': action, 'run_id': run, **fields})
        assert event['type'] == 'execution_response', event
        return event['response']
    def close(self):
        if self in clients:
            clients.remove(self)
        (f.ROOT / f'events-{id(self)}.json').write_text(json.dumps(self.events, indent=2))
        self.stream.close(); self.sock.close()

def submit(session, content, origin='human', delivery='next_turn', display_role=None):
    c = Client()
    ident = str(uuid.uuid4())
    envelope = {'id': ident, 'session': session, 'content': content, 'delivery': delivery, 'urgent': False}
    if origin:
        envelope['origin'] = {'kind': origin}
    if display_role:
        envelope['display_role'] = display_role
    reply = c.rpc('primary_input', input=envelope)
    c.close()
    return ident, reply['receipt']

def input_state(session, ident):
    c = Client()
    try:
        return c.rpc('primary_input_inspect', session=session, input=ident)['receipt']['state']
    finally:
        c.close()

def posts_with(text):
    return [p for p in f.posts if text in p['body']]

def settle_posts(count, seconds=4):
    wait(lambda: len(f.posts) >= count, f'{count} provider requests', 40)
    time.sleep(seconds)
    assert len(f.posts) == count, f'expected exactly {count} provider requests, saw {len(f.posts)}'

def recoveries():
    supervision = status().get('supervision') or {}
    return supervision.get('recoveries', [])

def unresolved():
    return [item for item in recoveries() if not item.get('resolved')]

def reviewed(action, *options):
    review = cli(action, *options)
    return cli('confirm', review['response']['value']['id'], '--request', review['confirm_request'])['response']['value']

def launchctl(*args):
    return subprocess.run(['/bin/launchctl', *args], capture_output=True, text=True, timeout=30)

def service_pid():
    printed = launchctl('print', f'gui/{uid}/{label}')
    if printed.returncode != 0:
        return None
    for line in printed.stdout.splitlines():
        if line.strip().startswith('pid = '):
            return int(line.strip()[6:])
    return None

def caffeinate_children(pid):
    rows = subprocess.run(['ps', '-axo', 'pid=,ppid=,command='], capture_output=True, text=True).stdout.splitlines()
    return [row for row in rows if row.split(None, 2)[1:2] == [str(pid)] and 'caffeinate' in row]

try:
    progress('start')
    p1 = start_daemon()
    c = Client(); worker = c.subscribe(); peer = Client().subscribe(); c.close()
    for client in list(clients):
        client.close()
    outcomes['sessions'] = {'worker': worker, 'idle_peer': peer}

    # R32: an invalid replacement is refused before any owned work is touched.
    # The running turn is not interrupted and needs no continuation.
    progress('invalid-reload')
    channel = f.home / 'builds' / 'shared-server'; channel.mkdir(parents=True)
    broken = channel / 'jcode'; broken.write_text('#!/bin/sh\nexit 3\n'); broken.chmod(0o755)
    submit(worker, 'HOLD:failed-reload work')
    settle_posts(1, 1)
    refused = run('server', 'reload', '--force', '--json', success=False)
    (f.ROOT / 'invalid-reload-cli.txt').write_text(refused.stdout + refused.stderr)
    time.sleep(2)
    (f.ROOT / 'release-failed-reload').write_text('release')
    settle_posts(1, 4)
    assert p1.poll() is None, 'invalid reload keeps the runtime serving'
    log = next((f.home / 'logs').glob('jcode-*.log')).read_text(errors='replace')
    assert 'replacement binary failed its smoke test' in log and 'Failed reload continued 0 interrupted turn(s)' in log
    assert unresolved() == []
    outcomes['invalid_reload_refused_before_effects'] = True
    broken.unlink()

    # R32: verified reload replaces the image in place and continues only the
    # interrupted turn; the idle peer stays idle; nothing is replayed.
    progress('reload')
    base = len(f.posts)
    submit(worker, 'HOLD:reload work')
    settle_posts(base + 1, 1)
    reloaded = run('server', 'reload', '--force', '--json', success=False)
    (f.ROOT / 'reload-cli.txt').write_text(reloaded.stdout + reloaded.stderr)
    (f.ROOT / 'release-reload').write_text('release')
    settle_posts(base + 2, 6)
    assert p1.poll() is None, 'reload must exec in place, keeping the process identity'
    assert 'interrupted by a server reload' in f.posts[-1]['body'] and 'HOLD:reload' in f.posts[-1]['body']
    assert unresolved() == [], 'a verified reload is not a crash'
    outcomes['reload_continues_interrupted_turn_once'] = True

    # R32: reviewed restart keeps desired-running and continues interrupted turns.
    progress('restart')
    base = len(f.posts)
    submit(worker, 'HOLD:restart work')
    settle_posts(base + 1, 1)
    before = status()['response']['value']['runtime']
    operation = reviewed('restart', '--strategy', 'interrupt', '--quiescence-seconds', '20')
    (f.ROOT / 'release-restart').write_text('release')
    restarted = cli('wait', operation['id'], '--timeout-seconds', '60')
    after = status()['response']['value']
    assert after['runtime'] != before and not after['desired_stopped'], after
    assert 'Restarted' in restarted.get('detail', ''), restarted
    settle_posts(base + 2, 6)
    assert 'HOLD:restart' in f.posts[-1]['body'] and 'interrupted' in f.posts[-1]['body']
    assert unresolved() == []
    outcomes['restart_new_incarnation_continues_once'] = {'before': before, 'after': after['runtime']}

    # R33: SIGKILL leaves interrupted turns for explicit selection. The daemon
    # comes back, inference does not; automatic wakes wait; two clients race.
    progress('crash')
    # An independent native command that outlives the crash under its own
    # owner is reported as live, so the user does not repeat it.
    effects = f.ROOT / 'live-effects'
    live_cmd = f.ROOT / 'live.py'
    live_cmd.write_text("import pathlib,sys,time\nroot=pathlib.Path(sys.argv[1])\nwith open(root/'live-effects','a') as e: e.write('x')\n(root/'live-ready').write_text('ready')\nwhile not (root/'live-release').exists(): time.sleep(0.1)\nprint('LIVE_DONE')\n")
    import sqlite3
    with sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro', uri=True) as db:
        before_runs = {row[0] for row in db.execute('SELECT id FROM runs')}
    shell_client = Client(); shell_client.subscribe(worker)
    shell_client.send('input_shell', command=f'{sys.executable} {live_cmd} {f.ROOT}')
    wait(lambda: (f.ROOT / 'live-ready').exists(), 'live command ready')
    with sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro', uri=True) as db:
        live_run = ({row[0] for row in db.execute("SELECT id FROM runs WHERE tool='input_shell'")} - before_runs).pop()
    assert shell_client.execution('background', live_run)['accepted']
    shell_client.close()
    base = len(f.posts)
    submit(worker, 'HOLD:crash-a work'); submit(peer, 'HOLD:crash-b work')
    settle_posts(base + 2, 1)
    p1.send_signal(signal.SIGKILL); p1.wait(timeout=10)
    (f.ROOT / 'release-crash-a').write_text('release'); (f.ROOT / 'release-crash-b').write_text('release')
    p2 = start_daemon()
    items = wait(lambda: len(unresolved()) == 2 and unresolved(), 'two recovery items')
    assert {item['cause'] for item in items} == {'unexpected_exit'}, items
    time.sleep(4); assert len(f.posts) == base + 2, 'no inference without a decision'
    wake, receipt = submit(peer, 'background completion fixture', origin=None, delivery='safe_boundary', display_role='background_task')
    time.sleep(3)
    assert input_state(peer, wake) == 'accepted' and len(f.posts) == base + 2, 'automatic wake deferred'
    worker_item = next(item for item in items if item['session'] == worker)
    live = [e for e in worker_item.get('executions', []) if e['id'] == live_run]
    assert live and live[0]['live_owner'], worker_item
    (f.ROOT / 'live-release').write_text('release')
    with sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro', uri=True) as db:
        wait(lambda: db.execute('SELECT state FROM runs WHERE id=?', (live_run,)).fetchone()[0] == 'completed', 'live command completes under its own owner')
    assert effects.read_text() == 'x', 'the surviving command ran exactly once'
    outcomes['crash_reports_live_native_owner'] = live_run
    results = []
    def decide():
        results.append(run('runtime', 'recover', 'continue', worker_item['id'], '--revision', str(worker_item['revision']), '--json', success=False))
    racers = [threading.Thread(target=decide) for _ in range(2)]
    [t.start() for t in racers]; [t.join() for t in racers]
    assert sorted(r.returncode == 0 for r in results) == [False, True], [(r.returncode, r.stderr[-400:]) for r in results]
    settle_posts(base + 3)
    assert 'the user chose to continue it' in f.posts[-1]['body'] and 'HOLD:crash-a' in f.posts[-1]['body']
    human, _ = submit(peer, 'human message after crash')
    wait(lambda: input_state(peer, human) == 'committed' and input_state(peer, wake) == 'committed', 'human then deferred wake delivered')
    peer_item = next(item for item in recoveries() if item['session'] == peer)
    assert peer_item['resolved']['resolution']['kind'] == 'superseded_by_input', peer_item
    late = run('runtime', 'recover', 'continue', peer_item['id'], '--json', success=False)
    assert late.returncode != 0, 'a superseded item cannot be continued later'
    time.sleep(3)
    assert not any('the user chose to continue it' in p['body'] and 'HOLD:crash-b' in p['body'] for p in f.posts)
    outcomes['crash_selected_recovery'] = {'posts': len(f.posts)}

    # R33: SIGTERM is a graceful interrupt, not an intentional Stop.
    progress('sigterm')
    count = len(f.posts)
    submit(worker, 'HOLD:sigterm work')
    settle_posts(count + 1, 1)
    p2.send_signal(signal.SIGTERM)
    assert p2.wait(timeout=40) == 0, 'graceful external termination exits successfully'
    (f.ROOT / 'release-sigterm').write_text('release')
    offline = status()
    assert not offline['response']['value']['desired_stopped'], offline
    p3 = start_daemon()
    items = wait(lambda: unresolved(), 'external-signal recovery item')
    assert [item['cause'] for item in items] == ['external_signal'], items
    left = cli('recover', 'leave', items[0]['id'])
    assert left['response']['value']['resolved']['resolution']['kind'] == 'left_stopped'
    time.sleep(4); assert len(f.posts) == count + 1, 'leave-stopped makes no provider call'
    outcomes['sigterm_external_signal_leave_stopped'] = True

    # R34/R35: isolated login service. Install while an unmanaged runtime runs,
    # hand over through a reviewed restart, then exercise supervision.
    if sys.platform == 'darwin':
        progress('service-install')
        broken_link = channel / 'jcode'
        os.symlink(f.BIN, broken_link)
        plan = cli('service', 'install')
        label = plan['label']
        assert label.startswith('dev.jcode.runtime.') and Path(plan['definition']).parent == agents_dir, plan
        assert plan['program'] == str(broken_link) and plan['arguments'][:2] == ['serve', '--socket'], plan
        assert not any(key for key in plan['environment'] if 'KEY' in key or 'TOKEN' in key), plan
        assert plan['environment']['HOME'] == os.environ['HOME'] and plan['environment']['JCODE_HOME'] == str(f.home), plan
        installed = cli('service', 'install', '--confirm', plan['digest'])
        assert installed['loaded'], installed
        waiting = wait(service_pid, 'service process waiting for the daemon lock')
        assert p3.poll() is None and connectable(), 'install never stops the running runtime'
        outcomes['service_waits_behind_unmanaged'] = waiting
        base = len(f.posts)
        submit(worker, 'HOLD:handoff work')
        settle_posts(base + 1, 1)
        held_count = len(f.posts)
        operation = reviewed('restart', '--strategy', 'interrupt', '--quiescence-seconds', '20')
        (f.ROOT / 'release-handoff').write_text('release')
        cli('wait', operation['id'], '--timeout-seconds', '90')
        assert p3.wait(timeout=20) == 0, 'unmanaged runtime hands over and exits cleanly'
        supervised = status()
        assert supervised['supervision']['supervised'], supervised
        settle_posts(held_count + 1, 6)
        assert 'HOLD:handoff' in f.posts[-1]['body'] and 'interrupted' in f.posts[-1]['body']
        outcomes['restart_handoff_to_service'] = True

        progress('service-competition')
        competitor = subprocess.run(f.args, env=f.env, capture_output=True, text=True, timeout=30)
        assert competitor.returncode != 0, 'no competing unmanaged daemon'
        racers = [threading.Thread(target=lambda: results.append(run('runtime', 'start', '--json', success=False))) for _ in range(2)]
        results.clear(); [t.start() for t in racers]; [t.join() for t in racers]
        assert all(r.returncode == 0 for r in results), [r.stderr[-400:] for r in results]
        pid = service_pid(); assert pid, 'service runs'
        refused = run('runtime', 'service', 'uninstall', '--json', success=False)
        assert refused.returncode != 0, 'uninstall refused while the supervised runtime runs'

        progress('power')
        count = len(f.posts)
        submit(worker, 'HOLD:power work')
        settle_posts(count + 1, 1)
        held = wait(lambda: caffeinate_children(pid), 'runtime power assertion while detached work runs', 20)
        assertions = subprocess.run(['pmset', '-g', 'assertions'], capture_output=True, text=True).stdout
        (f.ROOT / 'pmset-held.txt').write_text(assertions)
        assert 'caffeinate' in assertions and 'PreventUserIdleSystemSleep' in assertions
        power = status()['supervision']['power']
        assert power['active'] and power['active_work'] >= 1, power
        (f.ROOT / 'release-power').write_text('release')
        wait(lambda: not caffeinate_children(pid), 'assertion released after work settles', 20)
        (f.ROOT / 'pmset-released.txt').write_text(subprocess.run(['pmset', '-g', 'assertions'], capture_output=True, text=True).stdout)
        outcomes['power_follows_runtime_work'] = {'held': held}

        # R35: the user switch is reread on every reconcile.
        progress('power-switch')
        original = (f.home / 'config.toml').read_text()
        (f.home / 'config.toml').write_text(original + '\n[power]\nprevent_sleep_while_streaming=false\n')
        time.sleep(8)
        count = len(f.posts)
        submit(worker, 'HOLD:switch work')
        settle_posts(count + 1, 1)
        time.sleep(8)
        assert not caffeinate_children(pid), 'disabled switch holds no assertion'
        power = status()['supervision']['power']
        assert not power['enabled'] and not power['active'] and power['active_work'] >= 1, power
        (f.ROOT / 'release-switch').write_text('release')
        (f.home / 'config.toml').write_text(original)
        settle_posts(count + 1, 3)
        outcomes['power_switch_respected'] = power

        # R35: a native command preserved across a reviewed Stop keeps its own
        # assertion after the runtime exits and releases it when it settles.
        progress('survivor-power')
        survivor = f.ROOT / 'survivor.py'
        survivor.write_text("import pathlib,sys,time\nroot=pathlib.Path(sys.argv[1])\n(root/'survivor-ready').write_text('ready')\nwhile not (root/'survivor-release').exists(): time.sleep(0.1)\nprint('SURVIVOR_DONE')\n")
        shell_client = Client(); shell_client.subscribe(worker)
        shell_client.send('input_shell', command=f'{sys.executable} {survivor} {f.ROOT}')
        wait(lambda: (f.ROOT / 'survivor-ready').exists(), 'survivor command ready')
        def worker_assertions():
            rows = subprocess.run(['ps', '-axo', 'pid=,ppid=,command='], capture_output=True, text=True).stdout.splitlines()
            workers = {row.split()[0] for row in rows if '__jcode-command-worker' in row}
            return [row for row in rows if 'caffeinate' in row and row.split()[1] in workers]
        held = wait(worker_assertions, 'native worker assertion', 20)
        operation = reviewed('stop', '--strategy', 'interrupt', '--tasks', 'keep-supported')
        cli('wait', operation['id'], '--timeout-seconds', '60')
        try: shell_client.close()
        except Exception: pass
        wait(lambda: service_pid() is None and not connectable(), 'runtime stopped with the survivor preserved', 30)
        assert worker_assertions(), 'preserved command keeps its own assertion after the runtime exits'
        (f.ROOT / 'pmset-survivor.txt').write_text(subprocess.run(['pmset', '-g', 'assertions'], capture_output=True, text=True).stdout)
        (f.ROOT / 'survivor-release').write_text('release')
        wait(lambda: not worker_assertions(), 'survivor assertion released when the command settles', 30)
        cli('start')
        wait(service_pid, 'service started again', 20)
        outcomes['survivor_power'] = {'held': held}

        progress('service-crash')
        pid = wait(service_pid, 'current supervised runtime')
        wait(connectable, 'supervised runtime serves', 30)
        os.kill(pid, signal.SIGKILL)
        relaunched = wait(lambda: (lambda p: p if p and p != pid else None)(service_pid()), 'launchd relaunches after an unexpected exit', 40)
        wait(connectable, 'relaunched runtime serves', 30)
        assert not status()['response']['value']['desired_stopped']
        outcomes['service_crash_relaunch'] = {'before': pid, 'after': relaunched}

        progress('service-stop')
        operation = reviewed('stop', '--strategy', 'interrupt')
        cli('wait', operation['id'], '--timeout-seconds', '60')
        time.sleep(15)
        assert service_pid() is None and not connectable(), 'intentional Stop is never relaunched'
        launchctl('kickstart', f'gui/{uid}/{label}')
        time.sleep(5)
        assert service_pid() is None and not connectable(), 'login-equivalent launch honours intentional Stop'
        assert status()['response']['value']['desired_stopped']
        cli('start')
        assert service_pid() and connectable()
        outcomes['service_desired_stop_and_start'] = True

        progress('service-sigterm')
        pid = service_pid()
        launchctl('kill', 'TERM', f'gui/{uid}/{label}')
        wait(lambda: service_pid() is None, 'service runtime exits on SIGTERM', 40)
        time.sleep(12)
        assert service_pid() is None, 'graceful termination is not relaunched'
        assert not status()['response']['value']['desired_stopped']
        launchctl('kickstart', f'gui/{uid}/{label}')
        wait(service_pid, 'login-equivalent launch starts a runtime that was not intentionally stopped', 20)
        wait(connectable, 'runtime serves', 30)
        outcomes['service_sigterm_then_login_start'] = True
except Exception:
    errors.append(traceback.format_exc())
finally:
    signal.alarm(180)
    progress('cleanup')
    (f.ROOT / 'survivor-release').write_text('cleanup release')
    (f.ROOT / 'live-release').write_text('cleanup release')
    for tag in ['failed-reload', 'reload', 'restart', 'crash-a', 'crash-b', 'sigterm', 'handoff', 'power', 'switch']:
        (f.ROOT / ('release-' + tag)).write_text('cleanup release')
    for client in list(clients):
        try: client.close()
        except Exception: pass
    try:
        if connectable():
            operation = reviewed('stop', '--strategy', 'interrupt')
            cli('wait', operation['id'], '--timeout-seconds', '60')
        if label:
            launchctl('bootout', f'gui/{uid}/{label}')
            definition = agents_dir / f'{label}.plist'
            if definition.exists():
                definition.unlink()
            assert launchctl('print', f'gui/{uid}/{label}').returncode != 0, 'fixture job unloaded'
            cleanup.append({'service_removed': label})
    except Exception as error:
        cleanup.append({'error': repr(error)})
    for proc in daemons:
        if proc.poll() is None:
            try: proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                cleanup.append({'error': f'fixture daemon {proc.pid} still alive'})
    f.http.shutdown(); f.log.close()
    signal.alarm(0)
    result = {'root': str(f.ROOT), 'binary': f.BIN, 'errors': errors, 'cleanup': cleanup, 'outcomes': outcomes,
              'provider_requests': len(f.posts), 'sha256': hashlib.file_digest(open(f.BIN, 'rb'), 'sha256').hexdigest()}
    (f.ROOT / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
if errors or any('error' in item for item in cleanup):
    raise SystemExit(1)
