#!/usr/bin/env python3
"""Owned real-daemon, bridge, TS and Rust SDK acceptance. No paid inference.

Use run_isolated_test.py, --binary, --artifact-dir, and explicit
JCODE_WP07_BRIDGE/JCODE_WP07_RUST_PROBE binaries built from the candidate.
"""
import hashlib, json, os, socket, subprocess, time, uuid
from pathlib import Path
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import test_instruction_manager as f

def rpc(action, expected, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type':'workspace','id':rid,'request':{'action':action,**fields}})
    event = f.until(lambda e: e.get('id') == rid and e.get('type') in ('workspace_response','error'))
    assert event['type'] == 'workspace_response', event
    response=event['response']; assert response['kind'] == expected, response
    return response['value']
def uid(): return str(uuid.uuid4())
def sha256(path):
    with open(path,'rb') as source: return hashlib.file_digest(source,'sha256').hexdigest()
def change(action, **fields):
    review=rpc('review','review',expected_revision=rpc('status','status')['revision'],change={'action':action,**fields})
    result=rpc('apply','receipt',request=uid(),review=review['id'])
    assert not result['issues'], result
    return result['targets'][0]
def reconnect():
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    f.client=socket.socket(socket.AF_UNIX); f.client.settimeout(30); f.client.connect(str(f.sockpath)); f.reader=f.client.makefile('rb')

def closeout(request): return rpc('closeout','closeout',request=request)

bridge=None; result=None; cleanup={'errors':[]}; bridge_log=open(f.ROOT/'bridge.log','wb')
try:
    (f.ROOT/'wp07-owner.json').write_text(json.dumps({'owner':uid(),'kind':'disposable-closeout-sdk-fixture'}))
    f.start()
    f.counter+=1; f.send({'type':'workspace_probe','id':f.counter})
    probe=f.until(lambda e:e.get('id')==f.counter and e.get('type')=='workspace_capabilities')
    assert probe.get('closeout_version')==2 and not probe['managed_rollout'], probe
    rpc('initialize','status',request=uid())
    project=change('create_project',name='fixture-project')
    repository=change('create_repository',name='fixture-repo',remotes=[])
    change('associate_repository',project=project['id'],repository=repository['id'])
    checkout=f.ROOT/'owned-checkout'; checkout.mkdir()
    f.git(checkout,'init','-q'); f.git(checkout,'commit','--allow-empty','-qm','fixture')
    size=16*1024*1024
    (checkout/'payload').write_bytes(b'P'*size)
    (checkout/'unique').write_text('unique fixture information\n')
    unusual='line\n\tname'
    (checkout/unusual).write_text('unusual filename contents')
    target=b'/missing/\xff'
    os.symlink(target,os.fsencode(checkout/'opaque-link'))
    location=change('register_location',name='fixture-checkout',path=str(checkout),registration={'kind':'checkout','home':{'kind':'project','id':project['id']},'repository':repository['id']})
    fixture={'location':location['id'],'checkout':str(checkout.resolve()),'revision':rpc('status','status')['revision'],'payload_bytes':size,'payload_sha256':sha256(checkout/'payload')}
    fixture.update(unusual_name=unusual,opaque_target=list(target))
    (f.ROOT/'fixture.json').write_text(json.dumps(fixture))
    bridge_binary=str(Path(os.environ['JCODE_WP07_BRIDGE']).resolve())
    bridge=subprocess.Popen([bridge_binary,str(f.ROOT/'api.sock'),str(f.sockpath)],env=f.env,stdout=bridge_log,stderr=bridge_log)
    deadline=time.monotonic()+20
    while not (f.ROOT/'api.sock').exists():
        assert bridge.poll() is None
        assert time.monotonic()<deadline
        time.sleep(.05)
    f.reader.close(); f.reader=None; f.client.close(); f.client=None
    env=f.env|{'JCODE_WP07_NATIVE_ROOT':str(f.ROOT),'JCODE_WP07_LOCATION':location['id']}
    node=subprocess.run(['node',str(Path(__file__).with_name('verify_workspace_closeout_sdk.mjs'))],env=env,capture_output=True,text=True,timeout=600)
    (f.ROOT/'node.log').write_text(node.stdout+node.stderr)
    assert node.returncode==0, node.stderr
    rust=subprocess.run([os.environ['JCODE_WP07_RUST_PROBE'],'--ignored','--exact','closeout_native_retained_history_after_removal','--nocapture'],env=env,capture_output=True,text=True,timeout=45)
    (f.ROOT/'rust.log').write_text(rust.stdout+rust.stderr)
    assert rust.returncode==0 and '1 passed' in rust.stdout,(rust.stdout,rust.stderr)
    progress=json.loads((f.ROOT/'sdk-progress.json').read_text())
    rust_history=json.loads((f.ROOT/'rust-sdk-history.json').read_text())
    assert rust_history==progress['history']
    assert progress['disconnected_while_running'] and progress['cancelled'] and progress['output_read']
    assert not list((f.home/'sessions').glob('*.json')), 'administration created a provisional Session'
    assert not f.posts, 'closeout must not call a provider'
    result={'binary':subprocess.check_output([f.BIN,'--version'],env=f.env,text=True).strip(),'artifact':str(f.ROOT),'location':location['id'],'operation':progress['record']['operation'],'sdk_actions':len(progress['actions']),'provider_requests':0,'real_disconnect':True,'real_stop':True,'rust_ts_history_equal':True,'managed_rollout':False}
    result['artifact_identities']={str(Path(item).resolve()):sha256(item) for item in [f.BIN,bridge_binary,os.environ['JCODE_WP07_RUST_PROBE'],Path(__file__).resolve().parents[1]/'sdk/typescript/dist/client.js',Path(__file__).resolve().parents[1]/'sdk/typescript/dist/closeout.js']}
finally:
    try:
        progress_path=f.ROOT/'sdk-progress.json'
        if progress_path.exists() and f.proc and f.proc.poll() is None:
            reconnect(); terminal={'completed','failed','cancelled','interrupted'}
            for action in json.loads(progress_path.read_text())['actions']:
                request={'action':'execution','request':action['request'],'control':{'action':'inspect','run_id':action['run_id']}}
                state=closeout(request)['value']['run']
                if state['state'] not in terminal:
                    closeout(request|{'control':{'action':'stop','run_id':action['run_id']}})
                    deadline=time.monotonic()+30
                    while state['state'] not in terminal and time.monotonic()<deadline:
                        time.sleep(.1); state=closeout(request)['value']['run']
                assert state['state'] in terminal and (state['complete'] or state['state']=='failed'), state
        cleanup['all_recorded_executions_terminal']=True
    except Exception as error: cleanup['errors'].append(repr(error))
    if f.reader: f.reader.close()
    if f.client: f.client.close()
    if bridge and bridge.poll() is None:
        bridge.terminate(); bridge.wait(timeout=15)
    if not cleanup['errors'] and f.proc and f.proc.poll() is None:
        f.proc.terminate(); f.proc.wait(timeout=15)
    cleanup.update(owned_daemon_terminal=f.proc is None or f.proc.poll() is not None,owned_bridge_terminal=bridge is None or bridge.poll() is not None,retained_fixture=str(f.ROOT))
    f.http.shutdown(); f.log.close(); bridge_log.close()
    (f.ROOT/'cleanup.json').write_text(json.dumps(cleanup,indent=2))
    (f.ROOT/'events.json').write_text(json.dumps(f.events,indent=2))
    (f.ROOT/'provider-posts.json').write_text(json.dumps(f.posts,indent=2))
    if result and not cleanup['errors']:
        (f.ROOT/'closeout-result.json').write_text(json.dumps(result,indent=2)); print(json.dumps(result))
    print('artifacts='+str(f.ROOT))
    assert not cleanup['errors'], cleanup
