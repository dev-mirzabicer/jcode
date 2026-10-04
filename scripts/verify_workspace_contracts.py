#!/usr/bin/env python3
"""SP-58-C01/WP-11 acceptance through an isolated real daemon, the curated
Harness bridge and the TypeScript SDK, with a scripted localhost provider.

Covers the public catalog route, agent workspace discovery/proposals and the
narrow agent closeout route. No user checkout, production state, external
provider or payment is used. Run through scripts/run_isolated_test.py.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, re, socket, sqlite3, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc = Path(tempfile.mkdtemp(prefix='wc-', dir=os.environ['JCODE_RUNTIME_DIR']))
old = str(f.sockpath); f.sockpath = ipc/'s.sock'
f.args = [str(f.sockpath) if x == old else x for x in f.args]; f.env['JCODE_SOCKET'] = str(f.sockpath)
config = f.home/'config.toml'
STAGED = config.read_text()
ROLLED_OUT = STAGED.replace('[features]', '[features]\nmanaged_primary_launch=true', 1)
jobs = {}; captured = []; errors = []; channels = []; bridge = None; bridge_log = None
result = {'status': 'failed', 'binary': f.BIN, 'root': str(f.ROOT)}
lock = threading.Lock()

def provider(self):
    try:
        request = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
        with lock:
            captured.append(request)
            text = '\n'.join(str(m.get('content', '')) for m in request.get('messages', []) if m.get('role') == 'user')
            matches = [(key, job) for key, job in jobs.items() if key in text]
            assert matches, 'Unplanned provider request'
            key, job = matches[-1]
            job['requests'].append(request)
            first = len(job['requests']) == 1
        if first and job['tools']:
            calls = [{'index': i, 'id': str(uuid.uuid4()), 'type': 'function', 'function': {'name': name, 'arguments': json.dumps(args)}} for i, (name, args) in enumerate(job['tools'])]
            delta = {'tool_calls': calls}; reason = 'tool_calls'
        else:
            delta = {'content': 'Synthetic workspace contract step complete'}; reason = 'stop'
        payload = ''.join('data: '+json.dumps({'id': 'wp11-fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': d, 'finish_reason': r}]})+'\n\n' for d, r in [(delta, None), ({}, reason)])+'data: [DONE]\n\n'
        body = payload.encode(); self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    except Exception:
        errors.append(traceback.format_exc())
f.FixtureProvider.do_POST = provider

def uid(): return str(uuid.uuid4())
def wait(check, label, seconds=180):
    deadline = time.monotonic()+seconds
    while time.monotonic() < deadline:
        value = check()
        if value: return value
        time.sleep(.05)
    f.capture_timeout_diagnostics(label)
    raise TimeoutError(label)
def connect():
    client = socket.socket(socket.AF_UNIX); client.settimeout(180); client.connect(str(f.sockpath))
    channel = (client, client.makefile('rb')); channels.append(channel); return channel
def close(channel):
    channel[1].close(); channel[0].close(); channels.remove(channel)
def rpc(channel, request_type, accept=None, **fields):
    f.counter += 1; identity = f.counter
    channel[0].sendall((json.dumps({'type': request_type, 'id': identity, **fields})+'\n').encode())
    while True:
        line = channel[1].readline(); assert line, 'Control connection closed'
        event = json.loads(line); f.events.append(event)
        if event.get('id') != identity or event.get('type') == 'ack': continue
        if accept is not None and event.get('type') not in accept: continue
        return event
def ws(action, **fields):
    event = rpc(admin, 'workspace', request={'action': action, **fields}); assert event['type'] == 'workspace_response', event
    return event['response']
def ok(response, kind):
    assert response['kind'] == kind, response
    return response['value']
def status(): return ok(ws('status'), 'status')
def change(action, **fields):
    review = ok(ws('review', expected_revision=status()['revision'], change={'action': action, **fields}), 'review')
    return ok(ws('apply', request=uid(), review=review['id']), 'receipt')['targets'][0]
def saved(session): return json.loads((f.home/'sessions'/f'{session}.json').read_text())
def attach(session, root):
    channel = connect()
    assert rpc(channel, 'subscribe', accept=('done', 'error'), working_dir=str(root), target_session_id=session, selfdev=False)['type'] == 'done'
    return channel
def git(root, *args):
    subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', *args], cwd=root, check=True, capture_output=True, env={**f.env, 'GIT_CONFIG_NOSYSTEM': '1'})

def turn(session, tools):
    key = 'WS-JOB-'+uid(); jobs[key] = {'tools': tools, 'requests': []}
    response = rpc(admin, 'primary_input', input={'id': uid(), 'session': session, 'delivery': 'safe_boundary', 'content': key})
    assert response['type'] == 'primary_input_receipt', response
    expected = 2 if tools else 1
    wait(lambda: len(jobs[key]['requests']) >= expected, 'native-turn-'+key)
    def sealed():
        with sqlite3.connect(f.home/'execution/index.sqlite') as db:
            return db.execute("SELECT count(*) FROM runs WHERE session_id=? AND state IN ('running','queued','stopping')", (session,)).fetchone()[0] == 0
    wait(sealed, 'tool-terminal-'+key)
    observer = attach(session, Path(saved(session)['working_dir']))
    wait(lambda: not rpc(observer, 'state').get('is_processing', True), 'turn-idle-'+key)
    close(observer)
    assert len(jobs[key]['requests']) == expected, jobs[key]
    return jobs[key]['requests']

def tool_results(request):
    """This turn's tool results in the follow-up request, in call order."""
    messages = request['messages']
    last_user = max(i for i, m in enumerate(messages) if m.get('role') == 'user' and 'WS-JOB-' in str(m.get('content', '')))
    return [m.get('content') for m in messages[last_user:] if m.get('role') == 'tool']
def body(text):
    """The JSON tool body after the model-visible timing header."""
    return json.JSONDecoder().raw_decode(text[text.index('{'):])[0]
def duration(text):
    match = re.match(r'\[tool timing: .*? duration=([0-9.]+)(ms|s)\]', text)
    return float(match.group(1)) / (1000 if match.group(2) == 'ms' else 1)
def advertised(request):
    return {tool['function']['name']: tool['function'] for tool in request.get('tools', [])}
def tool_names(definitions): return {definition['name'] for definition in definitions}
def tool(name, **args): return name, {'intent': 'Verify owned workspace contract fixture', **args}

def sdk(program, label, seconds=120):
    module = (Path(__file__).resolve().parents[1]/'sdk/typescript/dist/index.js').as_uri()
    source = f"import assert from 'node:assert/strict';import {{JcodeClient,HarnessError}} from {json.dumps(module)};\nconst c=await JcodeClient.connect({{socketPath:{json.dumps(str(api))},requestTimeoutMs:120000,ensureRuntime:false}});\ntry{{{program}}}finally{{c.close();}}"
    (f.ROOT/f'{label}.mjs').write_text(source)
    run = subprocess.run(['node', '--input-type=module', '-e', source], env=f.env, capture_output=True, text=True, timeout=seconds)
    (f.ROOT/f'{label}.stdout').write_text(run.stdout); (f.ROOT/f'{label}.stderr').write_text(run.stderr)
    assert run.returncode == 0, run.stderr
    return json.loads(run.stdout.strip().splitlines()[-1])

def stop_daemon():
    global bridge
    for channel in list(channels): close(channel)
    if bridge and bridge.poll() is None: bridge.terminate(); bridge.wait(timeout=20)
    bridge = None
    f.reader.close(); f.client.close(); f.proc.terminate(); f.proc.wait(timeout=60)
def start_bridge():
    global bridge, bridge_log, api
    api = ipc/'api.sock'
    if api.exists(): api.unlink()
    bridge_log = (f.ROOT/'bridge.log').open('ab')
    bridge = subprocess.Popen([f.BIN, '--no-update', '--no-selfdev', '--socket', str(f.sockpath), 'api-bridge', '--api-socket', str(api)], env=f.env, stdout=bridge_log, stderr=bridge_log)
    wait(api.exists, 'bridge-ready')

try:
    # Phase 1: staged rollout (Mirza's real configuration). No session is
    # placed, so no session is offered the workspace tool.
    config.write_text(STAGED)
    f.start(); admin = connect()
    legacy = f.subscribe()
    first = turn(legacy, [])[0]
    assert 'workspace' not in advertised(first), sorted(advertised(first))
    result['staged_unplaced_tool_absent'] = True
    stop_daemon()

    # Phase 2: fixture-enabled rollout, as WP-12 will enable it.
    config.write_text(ROLLED_OUT)
    f.start(); admin = connect(); start_bridge()
    ok(ws('initialize', request=uid()), 'status')
    roots = {name: f.ROOT/name for name in ('a', 'b', 'outside', 'repo')}
    for root in roots.values(): root.mkdir()
    project = change('create_project', name='alpha')
    area = change('create_work_area', project=project['id'], name='sprint')
    repository = change('create_repository', name='api', remotes=[])
    change('associate_repository', project=project['id'], repository=repository['id'])
    loc_a = change('register_location', name='a', path=str(roots['a']), registration={'kind': 'directory', 'home': {'kind': 'work_area', 'id': area['id']}})
    loc_b = change('register_location', name='b', path=str(roots['b']), registration={'kind': 'directory', 'home': {'kind': 'project', 'id': project['id']}})
    change('register_location', name='outside', path=str(roots['outside']), registration={'kind': 'standalone'})
    git(roots['repo'], 'init', '-q'); git(roots['repo'], 'commit', '--allow-empty', '-qm', 'base')
    (roots['repo']/'payload').write_text('retained fixture bytes')
    loc_repo = change('register_location', name='repo', path=str(roots['repo']), registration={'kind': 'checkout', 'home': {'kind': 'project', 'id': project['id']}, 'repository': repository['id']})
    BULK = 23
    for index in range(BULK):
        path = f.ROOT/'bulk'/f'{index:02}'; path.mkdir(parents=True)
        change('register_location', name=f'bulk-{index:02}', path=str(path), registration={'kind': 'directory', 'home': {'kind': 'project', 'id': project['id']}})

    # Deliberate tool-prefix activation: the legacy session receives the tool
    # once, through an appended tool-set change, after reviewed adoption.
    legacy_root = change('register_location', name='legacy', path=str(f.project), registration={'kind': 'standalone'})
    before_adoption = saved(legacy)
    assert 'workspace' not in tool_names(before_adoption.get('tool_set', {}).get('advertised', []))
    adopted = rpc(admin, 'primary_location', command={'action': 'adopt_legacy', 'request': {'request': uid(), 'session': legacy, 'expected_working_dir': before_adoption['working_dir'], 'expected_catalog_revision': status()['revision'], 'placement': {'kind': 'standalone', 'id': legacy_root['id']}, 'cwd': str(f.project)}})
    assert adopted['type'] == 'primary_location_response' and adopted['response']['status'] == 'state', adopted
    wait(lambda: saved(legacy).get('location'), 'legacy-adoption')
    adopted_requests = turn(legacy, [])
    assert 'workspace' in advertised(adopted_requests[0])
    tool_set = saved(legacy)['tool_set']
    assert 'workspace' not in tool_names(tool_set['advertised'])
    def workspace_changes(changes):
        return [c['change'] for c in changes if c['change'].get('name') == 'workspace' or c['change'].get('definition', {}).get('name') == 'workspace']
    added = workspace_changes(tool_set['changes'])
    assert len(added) == 1 and added[0]['kind'] == 'added', tool_set['changes']
    result['unrelated_tool_set_changes'] = [c['change'] for c in tool_set['changes'] if c['change'] not in added]
    assert saved(legacy)['messages'][:len(before_adoption['messages'])] == before_adoption['messages']
    again = turn(legacy, [])[0]
    assert advertised(again)['workspace'] == advertised(adopted_requests[0])['workspace']
    assert len(workspace_changes(saved(legacy)['tool_set']['changes'])) == 1
    result['adoption_tool_set_change_once'] = True

    # C02 consumer: managed launch and durable input through the public SDK.
    launched = sdk(f"""
const versions=await c.workspaceCapabilities();assert.equal(versions.management_version,1);
const status=await c.workspace({{action:'status'}});assert.equal(status.kind,'status');
const record=await c.launchPrimary({{request:crypto.randomUUID(),expected_revision:status.value.revision,input:{{placement:{{kind:'existing',placement:{{kind:'directory',id:{json.dumps(loc_a['id'])}}}}},cwd:{{kind:'existing',path:{json.dumps(str(roots['a']))}}},agent:null,model:null,selfdev:false}}}});
const view=await c.primaryLocation({{action:'inspect_session',session:record.session}});
assert.equal(view.status,'session');assert.equal(view.view.location.placement.id,{json.dumps(loc_a['id'])});
console.log(JSON.stringify({{session:record.session}}));""", 'c02-launch')
    session = launched['session']

    # Initial context names placement and home chain once, after Startup Context.
    context = json.dumps(saved(session)['messages'])
    for identity in (loc_a['id'], area['id'], project['id']):
        assert identity in context, identity
    result['initial_location_facts'] = True

    # Discovery, paging and the first frozen tool set.
    (roots['a']/'probe.txt').write_text('timing comparison')
    requests = turn(session, [tool('workspace', action='inspect'), tool('workspace', action='list', kind='project', id=project['id'], limit=10), tool('workspace', action='locate', path=str(roots['b']/'x.txt')), tool('read', file_path=str(roots['a']/'probe.txt'))])
    result['tool_seconds'] = [duration(text) for text in tool_results(requests[1])]
    tools = advertised(requests[0]); assert 'workspace' in tools
    frozen = saved(session)
    assert frozen.get('workspace_guidance') == tools['workspace']['description']
    inspected, page, located = [body(text) for text in tool_results(requests[1])[:3]]
    assert area['id'] in json.dumps(inspected['work_area']) and project['id'] in json.dumps(inspected['project']), inspected
    assert page['total'] == BULK + 5 and len(page['items']) == 10 and page['next'], page
    assert located['location']['id'] == loc_b['id'] and located['writable'] is False, located
    # Continue the agent's page with its exact continuation.
    seen = {item['value']['id'] for item in page['items']}; after = page['next']
    while after:
        more = body(tool_results(turn(session, [tool('workspace', action='list', kind='project', id=project['id'], limit=10, after=json.dumps(after))])[1])[0])
        seen |= {item['value']['id'] for item in more['items']}; after = more['next']
    assert len(seen) == BULK + 5, len(seen)
    result['paged_discovery'] = {'total': BULK + 5, 'page': 10}

    # A proposal with forged authority fields is pending and grants nothing.
    turn(session, [tool('write', file_path=str(roots['b']/'denied.txt'), content='denied')])
    assert not (roots['b']/'denied.txt').exists()
    proposed = body(tool_results(turn(session, [tool('workspace', action='request_access', kind='location', id=loc_b['id'], reason='Synthetic cross-root task', approved=True, trusted=True, session='session_forged', grant=uid())])[1])[0])
    proposal = proposed['proposal']
    assert proposal['session'] == session and proposal['state'] == 'pending' and not proposal.get('grant'), proposal
    turn(session, [tool('write', file_path=str(roots['b']/'still-denied.txt'), content='denied')])
    assert not (roots['b']/'still-denied.txt').exists()
    result['untrusted_proposal_not_authority'] = True

    # C04 consumer: the trusted client finds and approves the proposal.
    approved = sdk(f"""
let pending=[];let after=null;
do{{const r=await c.workspace({{action:'permissions',request:{{action:'list',query:{{kind:'proposals',session:null,state:'pending'}},after,limit:1}}}});assert.equal(r.kind,'permissions');pending.push(...r.value.value.items);after=r.value.value.next;}}while(after);
const item=pending.find(p=>p.value.id==={json.dumps(proposal['id'])});assert(item);
const status=await c.workspace({{action:'status'}});
const review=await c.workspace({{action:'permissions',request:{{action:'review',expected_revision:status.value.revision,change:{{action:'issue',audience:{{kind:'session',id:{json.dumps(session)}}},target:item.value.target,proposal:item.value.id}}}}}});
assert.equal(review.value.kind,'review');
const request=crypto.randomUUID();
const applied=await c.workspace({{action:'permissions',request:{{action:'apply',request,review:review.value.value.id}}}});
assert.equal(applied.value.value.proposal.state,'approved');
const ops=await c.workspace({{action:'operations',query:{{session:{json.dumps(session)}}},after:null,limit:20}});assert.equal(ops.kind,'operations');
await assert.rejects(()=>c.workspace({{action:'closeout',request:{{action:'history',location:{json.dumps(loc_a['id'])}}}}}),e=>e instanceof HarnessError&&e.code==='invalid_request');
console.log(JSON.stringify({{grant:applied.value.value.grant.id,operations:ops.value.total}}));""", 'c04-approve')
    status_after = body(tool_results(turn(session, [tool('workspace', action='access_status', proposal=proposal['id'])])[1])[0])
    assert status_after['state'] == 'approved', status_after
    turn(session, [tool('write', file_path=str(roots['b']/'allowed.txt'), content='granted')])
    assert (roots['b']/'allowed.txt').read_text() == 'granted'
    result['trusted_approval_then_write'] = approved

    # Agent closeout under human-issued conditional authorization.
    closer = sdk(f"""
const status=await c.workspace({{action:'status'}});
const record=await c.launchPrimary({{request:crypto.randomUUID(),expected_revision:status.value.revision,input:{{placement:{{kind:'existing',placement:{{kind:'project',id:{json.dumps(project['id'])}}}}},cwd:{{kind:'existing',path:{json.dumps(str(roots['b']))}}},agent:null,model:null,selfdev:false}}}});
const current=await c.workspace({{action:'status'}});
const begun=await c.closeout({{action:'begin',request:crypto.randomUUID(),expected_revision:current.value.revision,spec:{{location:{json.dumps(loc_repo['id'])},expected_generation:1,preservation_directory:null,conditional_no_loss:true,full_archive:true}}}});
console.log(JSON.stringify({{session:record.session,operation:begun.response.value.operation}}));""", 'closeout-begin')
    closer_session, operation = closer['session'], closer['operation']
    def closeout_record():
        reply = rpc(admin, 'workspace', request={'action': 'closeout', 'request': {'action': 'inspect', 'operation': operation}})
        return ok(ok(reply['response'], 'closeout'), 'record')
    def step(name, **fields):
        record = closeout_record()
        out = body(tool_results(turn(closer_session, [tool('workspace', action='closeout_action', operation=operation, expected_revision=record['revision'], step=name, **fields)])[1])[0])
        return out
    for name in ('refresh', 'preserve'):
        out = step(name); assert out['actor'] == 'agent' and out['issue'] is None, out
    reviewed = step('review_removal'); review = reviewed['result']['value']
    assert not review['issues'], review
    approve = turn(closer_session, [tool('workspace', action='closeout_action', operation=operation, expected_revision=closeout_record()['revision'], step='approve_removal', review=review['id'])])
    assert 'approve_removal' in json.dumps(approve[1]) and closeout_record()['stage'] == 'ready_for_approval'
    declared = step('declare_no_loss', review=review['id'], assessment='Synthetic: every inventory entry is preserved and verified')
    assert declared['result']['value']['stage'] == 'authorized', declared
    finished = step('finish')
    assert finished['result']['value']['stage'] == 'closed' and not roots['repo'].exists(), finished
    result['agent_conditional_closeout'] = {'operation': operation, 'session': closer_session}

    # Restart: the frozen tool set and guidance are reused byte-for-byte.
    before = advertised(requests[0])['workspace']
    stop_daemon(); f.start(); admin = connect()
    resumed = turn(session, [])[0]
    assert advertised(resumed)['workspace'] == before
    result['restart_tool_prefix_stable'] = True
    assert not errors, errors
    result.update(status='passed', provider_calls=len(captured), session=session)
except Exception:
    result['error'] = traceback.format_exc()
finally:
    for channel in list(channels): close(channel)
    if bridge and bridge.poll() is None:
        bridge.terminate()
        try: bridge.wait(timeout=20)
        except subprocess.TimeoutExpired: bridge.kill(); bridge.wait(timeout=10)
    if bridge_log: bridge_log.close()
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try: f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired: f.proc.kill(); f.proc.wait(timeout=10)
    f.http.shutdown(); f.http.server_close(); f.log.close()
    (f.ROOT/'contracts-result.json').write_text(json.dumps(result, indent=2))
    (f.ROOT/'contracts-events.json').write_text(json.dumps(f.events, indent=2))
    (f.ROOT/'contracts-provider.json').write_text(json.dumps(captured, indent=2))
    (f.ROOT/'cleanup.json').write_text(json.dumps({'daemon_terminal': f.proc is None or f.proc.poll() is not None, 'bridge_terminal': bridge is None or bridge.poll() is not None}))
    print(json.dumps(result)); print('artifacts='+str(f.ROOT))
if result['status'] != 'passed': raise SystemExit(1)
