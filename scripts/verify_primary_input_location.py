#!/usr/bin/env python3
"""Owned daemon/input/location acceptance with localhost scripted provider only."""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, shlex, socket, sqlite3, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc = Path(tempfile.mkdtemp(prefix='pi-', dir=os.environ['JCODE_RUNTIME_DIR']))
old_socket = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if arg == old_socket else arg for arg in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
config = f.home / 'config.toml'
config.write_text(config.read_text().replace('[features]', '[features]\nmanaged_primary_launch=true', 1))
roots = [f.ROOT / name for name in ('initial', 'second', 'repair')]
for root in roots:
    root.mkdir()
release = f.ROOT / 'release'
release_background = f.ROOT / 'release-background'
entered = f.ROOT / 'entered'
channels, captured, failures = [], [], []
provider_lock = threading.Lock()
recovery_started = threading.Event()
recovery_release = threading.Event()
expected_crash = False
result = {'status': 'failed', 'binary': f.BIN, 'root': str(f.ROOT)}
bridge = None
bridge_log = None


def tool(index, name, command, background=False):
    return {'index': index, 'id': name, 'type': 'function', 'function': {'name': 'bash', 'arguments': json.dumps({'command': command, 'intent': 'Verify owned input/location lifecycle', 'run_in_background': background, 'notify': False, 'wake': False, 'output_size': 'small', 'timeout': 120000})}}


def complete(self):
    try:
        request = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        with provider_lock:
            captured.append(request)
            number = len(captured)
        user_text = '\n'.join(str(message.get('content','')) for message in request.get('messages',[]) if message.get('role') == 'user')
        if 'RECOVERY-INITIAL' in user_text and 'RECOVERY-QUEUED' not in user_text:
            recovery_started.set()
            assert recovery_release.wait(120), 'fixture recovery stream was not released'
        q = shlex.quote
        if number == 1:
            delta = {'tool_calls': [
                tool(0, 'original-background', f'while [ ! -f {q(str(release_background))} ]; do sleep .05; done; pwd > background-cwd.txt; printf x >> background-effects.txt', True),
                tool(1, 'original-blocking', f'touch {q(str(entered))}; while [ ! -f {q(str(release))} ]; do sleep .05; done; pwd > first-cwd.txt; printf x >> first-effects.txt'),
                tool(2, 'same-batch', 'pwd > second-cwd.txt; printf x >> second-effects.txt'),
            ]}
            finish = 'tool_calls'
        elif number == 2:
            delta = {'tool_calls': [tool(0, 'new-location', 'pwd > after-cwd.txt; printf x >> after-effects.txt')]}
            finish = 'tool_calls'
        else:
            delta = {'content': 'Synthetic primary input complete'}
            finish = 'stop'
        chunks = [
            {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
            {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]},
        ]
        payload = (''.join('data: '+json.dumps(chunk)+'\n\n' for chunk in chunks)+'data: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    except (BrokenPipeError, ConnectionResetError):
        if not expected_crash:
            failures.append(traceback.format_exc())
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


def rpc(channel, kind, **fields):
    f.counter += 1
    request = f.counter
    channel[0].sendall((json.dumps({'type': kind, 'id': request, **fields})+'\n').encode())
    while True:
        try:
            line = channel[1].readline()
        except TimeoutError:
            f.capture_timeout_diagnostics('primary-control-'+kind)
            raise
        assert line, 'daemon closed request channel'
        event = json.loads(line)
        f.events.append(event)
        if event.get('id') == request and event.get('type') != 'ack':
            if kind in ('subscribe', 'clear') and event.get('type') not in ('done', 'error'):
                continue
            return event


def ws(action, **fields):
    event = rpc(admin, 'workspace', request={'action': action, **fields})
    assert event['type'] == 'workspace_response', event
    assert event['response']['kind'] != 'error', event
    return event['response']['value']


def wait(check, label, seconds=120):
    deadline = time.monotonic()+seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise TimeoutError(label)


def close(channel):
    channel[1].close()
    channel[0].close()
    channels.remove(channel)


def move(location, cwd, revision):
    request = {'request': str(uuid.uuid4()), 'session': session, 'expected_session_revision': revision,
               'expected_catalog_revision': ws('status')['revision'], 'placement': location, 'cwd': str(cwd)}
    event = rpc(admin, 'primary_location', command={'action': 'change', 'request': request})
    assert event['type'] == 'primary_location_response' and event['response']['status'] == 'state', event
    return event['response']['record'], request


def inspect_input(input_id):
    event = rpc(admin, 'primary_input_inspect', session=session, input=input_id)
    assert event['type'] == 'primary_input_receipt', event
    return event['receipt']


def execution_terminal():
    database = f.home / 'execution/index.sqlite'
    if not database.exists():
        return False
    with sqlite3.connect(database) as db:
        rows = db.execute("SELECT state FROM runs WHERE session_id=? AND tool='bash'", (session,)).fetchall()
    return all(row[0] in ('completed', 'cancelled', 'failed', 'interrupted') for row in rows)

try:
    f.start()
    admin = connect()
    ws('initialize', request=str(uuid.uuid4()))
    locations = []
    for root in roots:
        review = ws('review', expected_revision=ws('status')['revision'], change={'action': 'register_location', 'name': root.name, 'path': str(root), 'registration': {'kind': 'standalone'}})
        receipt = ws('apply', request=str(uuid.uuid4()), review=review['id'])
        locations.append({'kind': 'standalone', 'id': receipt['targets'][0]['id']})
    launched = rpc(admin, 'primary_launch', request={'request': str(uuid.uuid4()), 'expected_revision': ws('status')['revision'], 'input': {'placement': {'kind': 'existing', 'placement': locations[0]}, 'cwd': {'kind': 'existing', 'path': str(roots[0])}, 'agent': None, 'model': None, 'selfdev': False}})
    assert launched['response']['status'] == 'launched', launched
    session = launched['response']['record']['session']
    original = json.loads((f.home/'sessions'/f'{session}.json').read_text())
    idle_move, _ = move(locations[1], roots[1], 1)
    assert idle_move['state'] == 'complete' and not captured, idle_move
    after_idle = json.loads((f.home/'sessions'/f'{session}.json').read_text())
    assert after_idle['messages'][:-1] == original['messages']
    assert after_idle['system_prompt'] == original['system_prompt']
    input_id = str(uuid.uuid4())
    input_body = {'id': input_id, 'session': session, 'delivery': 'safe_boundary', 'content': 'NATIVE-DURABLE-PRIMARY-INPUT', 'images': []}
    # Deliberately lose the acceptance reply, then retry its durable identity.
    lost = connect()
    lost[0].sendall((json.dumps({'type':'primary_input', 'id':77, 'input':input_body})+'\n').encode())
    close(lost)
    wait(entered.exists, 'original native tool did not start')
    replay = rpc(admin, 'primary_input', input=input_body)
    assert replay['receipt']['state'] == 'committed', replay
    conflict = rpc(admin, 'primary_input', input={**input_body, 'content':'CONFLICTING-INPUT'})
    assert conflict['type'] == 'error', conflict
    busy_move, busy_request = move(locations[0], roots[0], 2)
    assert busy_move['state'] == 'pending', busy_move
    safe_id = str(uuid.uuid4())
    assert rpc(admin, 'primary_input', input={'id':safe_id, 'session':session, 'delivery':'safe_boundary', 'content':'SAFE-BATCH-INPUT'})['receipt']['state'] == 'accepted'
    release.write_text('owned fixture release')
    wait(lambda: (roots[0]/'after-cwd.txt').exists(), 'subsequent native tool did not use new cwd')
    release_background.write_text('owned background release')
    wait(lambda: (roots[1]/'background-cwd.txt').exists(), 'background cwd was not retained')
    for name in ('first-cwd.txt','second-cwd.txt','background-cwd.txt'):
        assert (roots[1]/name).read_text().strip() == str(roots[1].resolve()), name
    assert (roots[0]/'after-cwd.txt').read_text().strip() == str(roots[0].resolve())
    wait(lambda: len(captured) == 3, 'provider tool workflow did not settle')
    wait(execution_terminal, 'owned execution rows were not sealed')
    with sqlite3.connect(f.home/'execution/index.sqlite') as db:
        runs = db.execute("SELECT state FROM runs WHERE session_id=? AND tool='bash'",(session,)).fetchall()
    assert len(runs) == 4 and all(row[0] == 'completed' for row in runs), runs
    assert inspect_input(safe_id)['state'] == 'committed'
    replay_move = rpc(admin,'primary_location',command={'action':'change','request':busy_request})
    # The replay can meet the control lease while its commit finishes; Busy is
    # transient and the same request is retried.
    while replay_move['response'].get('status') == 'rejected' and replay_move['response']['issue']['code'] == 'busy':
        time.sleep(0.2)
        replay_move = rpc(admin,'primary_location',command={'action':'change','request':busy_request})
    assert replay_move['response']['record']['state'] == 'complete'
    assert len(captured) == 3 and not failures
    second_messages = captured[1]['messages']
    assert sum('SAFE-BATCH-INPUT' in str(message.get('content','')) for message in second_messages) == 1
    results = [index for index,message in enumerate(second_messages) if message.get('role') == 'tool']
    control_id = replay_move['response']['record']['notice_message']
    checkpoint = json.loads((f.home/'sessions'/f'{session}.json').read_text())
    control = next(message for message in checkpoint['messages'] if message['id'] == control_id)
    control_text = next(block['text'] for block in control['content'] if block['type'] == 'text')
    notice = [index for index,message in enumerate(second_messages) if control_text in str(message.get('content',''))]
    assert len(results) >= 3 and notice[-1] > max(results), second_messages
    # Restart must not repeat committed input or native effects.
    before_restart = json.loads((f.home/'sessions'/f'{session}.json').read_text())
    assert before_restart['system_prompt']['text'] == original['system_prompt']['text']
    assert before_restart['system_prompt']['active_agent'] == original['system_prompt']['active_agent']
    assert before_restart['system_prompt'].get('first_provider_dispatch_at')
    for channel in list(channels): close(channel)
    f.reader.close(); f.client.close(); f.proc.terminate(); f.proc.wait(timeout=30)
    roots[0].rename(f.ROOT/'old-cwd-retained')
    f.start(); admin = connect()
    repaired, _ = move(locations[2],roots[2],3)
    assert repaired['state'] == 'complete', repaired
    stored = json.loads((f.home/'sessions'/f'{session}.json').read_text())
    assert stored['working_dir'] == str(roots[2].resolve())
    assert stored['location']['initial_cwd'] == original['location']['initial_cwd']
    assert stored['system_prompt'] == before_restart['system_prompt']
    assert not roots[0].exists() and len(captured) == 3
    for effect in ('background', 'first', 'second'):
        assert (roots[1]/f'{effect}-effects.txt').read_text() == 'x'
    assert (f.ROOT/'old-cwd-retained'/'after-effects.txt').read_text() == 'x'
    assert inspect_input(input_id)['state'] == 'committed'
    detail = rpc(admin, 'primary_input_read', session=session, input=input_id)
    assert detail['input']['content'] == input_body['content'] and detail['receipt']['state'] == 'committed'
    attached = connect()
    assert rpc(attached, 'subscribe', working_dir=str(roots[2]), target_session_id=session, selfdev=False)['type'] == 'done'
    changed = rpc(attached, 'set_agent', agent='global:jcode', replace=True)
    assert changed['type'] == 'agent_selected', changed
    replaced = json.loads((f.home/'sessions'/f'{session}.json').read_text())
    assert replaced['messages'][:len(original['messages'])] == original['messages']
    assert replaced['location'] == stored['location'] and len(captured) == 3
    split = rpc(attached, 'split')
    assert split['type'] == 'split_response', split
    clone = json.loads((f.home/'sessions'/f"{split['new_session_id']}.json").read_text())
    # A split publishes its own location record (own revision and operation,
    # WP-05 scoped contexts) for the same placement, cwd and historical cwd.
    for key in ('placement', 'cwd', 'initial_cwd'):
        assert clone['location'][key] == replaced['location'][key], key
    assert clone['system_prompt'] == replaced['system_prompt']
    assert not clone.get('primary_inputs')
    assert rpc(attached, 'clear')['type'] == 'done'
    fresh_id = [event['session_id'] for event in f.events if event['type'] == 'session'][-1]
    assert fresh_id != session
    fresh = json.loads((f.home/'sessions'/f'{fresh_id}.json').read_text())
    assert fresh['working_dir'] == str(roots[2].resolve()) and fresh['location']['initial_cwd'] == str(roots[2].resolve())
    assert not fresh.get('primary_inputs') and len(captured) == 3
    # A second primary accepts next-turn input while an earlier provider call
    # is held. Crash the owned daemon before that queued input can commit.
    recovery_initial = {'id':str(uuid.uuid4()),'session':fresh_id,'delivery':'safe_boundary','content':'RECOVERY-INITIAL'}
    assert rpc(admin,'primary_input',input=recovery_initial)['receipt']['state'] in ('accepted','committed')
    assert recovery_started.wait(30), 'recovery fixture provider did not start'
    queued = {'id':str(uuid.uuid4()),'session':fresh_id,'delivery':'next_turn','content':'RECOVERY-QUEUED'}
    assert rpc(admin,'primary_input',input=queued)['receipt']['state'] == 'accepted'
    assert rpc(admin,'primary_input_inspect',session=fresh_id,input=queued['id'])['receipt']['state'] == 'accepted'
    expected_crash = True
    f.proc.kill(); f.proc.wait(timeout=30)
    for channel in list(channels): close(channel)
    f.reader.close(); f.client.close()
    recovery_release.set()
    f.start(); admin = connect()
    # An unexpected exit restores the runtime but never resumes inference by
    # itself (WP-09, R33): the interrupted primary becomes a recovery item and
    # its accepted input waits, durable, for a trusted decision.
    def runtime(*args):
        out = subprocess.run([f.BIN,'--no-update','--no-selfdev','--socket',str(f.sockpath),'runtime',*args,'--json'],env=f.env,capture_output=True,text=True,timeout=90)
        assert out.returncode == 0, (args, out.stdout[-2000:], out.stderr[-2000:])
        return json.loads(out.stdout)
    item = wait(lambda: next((i for i in (runtime('status').get('supervision') or {}).get('recoveries', []) if i['session'] == fresh_id and not i.get('resolved')), None), 'crash recovery item for the interrupted primary')
    time.sleep(2)
    assert rpc(admin,'primary_input_inspect',session=fresh_id,input=queued['id'])['receipt']['state'] == 'accepted'
    assert len(captured) == 4, 'no inference before the recovery decision'
    runtime('recover','continue',item['id'])
    wait(lambda: rpc(admin,'primary_input_inspect',session=fresh_id,input=queued['id'])['receipt']['state'] == 'committed', 'accepted input did not recover after the continue decision')
    wait(lambda: any('RECOVERY-QUEUED' in str(m.get('content','')) for m in captured[-1]['messages']), 'recovery did not dispatch the pending input')
    assert rpc(admin,'primary_input',input=queued)['receipt']['state'] == 'committed'
    # The Session checkpoint records each input once: the two accepted
    # inputs and the runtime-owned continuation that the decision delivered.
    def persisted_inputs():
        inputs = json.loads((f.home/'sessions'/f'{fresh_id}.json').read_text()).get('primary_inputs') or []
        return inputs if len(inputs) == 3 else None
    inputs = wait(persisted_inputs, 'recovered inputs checkpointed once each')
    (f.ROOT/'recovered-primary-inputs.json').write_text(json.dumps(inputs, indent=1))
    ids = [entry['id'] for entry in inputs]
    assert len(set(ids)) == 3 and {recovery_initial['id'], queued['id']} <= set(ids), inputs
    assert not any(entry.get('rolled_back') for entry in inputs), inputs
    assert sum('RECOVERY-QUEUED' in str(message.get('content','')) for message in captured[-1]['messages']) == 1
    assert not failures
    # The selected continuation and the queued input are the only calls after
    # the decision; the SDK checks below must add none.
    def settled():
        before = len(captured); time.sleep(3)
        return len(captured) if len(captured) == before else None
    recovered_calls = wait(settled, 'recovered turns settle')
    # One selected continuation, then the queued input: no replay.
    assert recovered_calls == 6, recovered_calls
    api_path = ipc/'api.sock'
    bridge_log = (f.ROOT/'primary-api-bridge.log').open('wb')
    bridge = subprocess.Popen([f.BIN,'--no-update','--no-selfdev','--socket',str(f.sockpath),'api-bridge','--api-socket',str(api_path)],env=f.env,stdout=bridge_log,stderr=bridge_log)
    wait(api_path.exists,'public bridge did not start',30)
    session_count = len(list((f.home/'sessions').glob('*.json')))
    module = (Path(__file__).resolve().parents[1]/'sdk/typescript/dist/index.js').as_uri()
    program = f"""
      import assert from 'node:assert/strict';
      import {{ JcodeClient }} from {json.dumps(module)};
      const client = await JcodeClient.connect({{socketPath:{json.dumps(str(api_path))}}});
      try {{
        const detail = await client.readPrimaryInput({json.dumps(session)},{json.dumps(input_id)});
        assert.equal(detail.receipt.state,'committed');
        assert.equal(detail.input.content,'NATIVE-DURABLE-PRIMARY-INPUT');
        const location = await client.primaryLocation({{action:'inspect',operation:{json.dumps(repaired['operation'])}}});
        assert.equal(location.status,'state'); assert.equal(location.record.state,'complete');
        const input = {{id:{json.dumps(str(uuid.uuid4()))},session:{json.dumps(session)},delivery:'context_only',content:'PUBLIC-CONTEXT-ONLY'}};
        await client.submitPrimaryInput(input);
        const deadline = Date.now()+30000;
        let receipt;
        do {{ receipt=await client.inspectPrimaryInput(input.session,input.id); if(receipt.state==='committed') break; await new Promise(resolve=>setTimeout(resolve,20)); }} while(Date.now()<deadline);
        assert.equal(receipt.state,'committed');
        assert.equal((await client.submitPrimaryInput(input)).state,'committed');
        console.log(JSON.stringify({{public_input:true,public_original:true,public_location:true}}));
      }} finally {{ client.close(); }}
    """
    public = subprocess.run(['node','--input-type=module','-e',program],env=f.env,text=True,capture_output=True,timeout=120)
    (f.ROOT/'public-client.stdout').write_text(public.stdout)
    (f.ROOT/'public-client.stderr').write_text(public.stderr)
    assert public.returncode == 0, public.stderr
    sessions_after = len(list((f.home/'sessions').glob('*.json')))
    assert sessions_after == session_count and len(captured) == recovered_calls, (sessions_after, session_count, len(captured), recovered_calls)
    result.update(public_sdk=json.loads(public.stdout))
    result.update(status='passed' , profile_replacement_history=True, split_location=True, clear_current_cwd=True, accepted_crash_recovery=True, session=session, provider_calls=len(captured), idle_no_inference=True,
                  lost_ack_replay=True, conflict_rejected=True, full_tool_batch_boundary=True,
                  subsequent_cwd=True, original_background_cwd=True, committed_no_restart_replay=True,
                  missing_cwd_repaired=True, frozen_system_unchanged=True)
except Exception:
    result.update(error=traceback.format_exc())
finally:
    recovery_release.set()
    release.write_text('cleanup'); release_background.write_text('cleanup')
    if 'session' in globals():
        try: wait(execution_terminal, 'cleanup terminal persistence', 20)
        except Exception: result.update(cleanup_error=traceback.format_exc(), status='failed')
    for channel in list(channels): close(channel)
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    if bridge and bridge.poll() is None:
        bridge.terminate()
        try: bridge.wait(timeout=15)
        except subprocess.TimeoutExpired: bridge.kill(); bridge.wait(timeout=5)
    if bridge_log: bridge_log.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try: f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired: f.proc.kill(); f.proc.wait(timeout=10)
    f.http.shutdown(); f.http.server_close(); f.log.close()
    (f.ROOT/'input-location-result.json').write_text(json.dumps(result,indent=2))
    (f.ROOT/'input-location-provider.json').write_text(json.dumps(captured,indent=2))
    (f.ROOT/'input-location-events.json').write_text(json.dumps(f.events,indent=2))
    (f.ROOT/'input-location-cleanup.json').write_text(json.dumps({'daemon_exit':f.proc.returncode if f.proc else None,'provider_errors':failures},indent=2))
    print(json.dumps(result,indent=2))
    if result['status'] != 'passed': raise SystemExit(1)
