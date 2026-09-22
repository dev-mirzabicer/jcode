#!/usr/bin/env python3
"""WP-05 acceptance through an isolated real daemon and localhost provider.
No user checkout, production state, external provider or payment is used.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, socket, sqlite3, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc=Path(tempfile.mkdtemp(prefix='ps-',dir=os.environ['JCODE_RUNTIME_DIR']))
old=str(f.sockpath); f.sockpath=ipc/'s.sock'; f.args=[str(f.sockpath) if x==old else x for x in f.args];f.env['JCODE_SOCKET']=str(f.sockpath)
config=f.home/'config.toml';config.write_text(config.read_text().replace('[features]','[features]\nmanaged_primary_launch=true',1))
roots=[f.ROOT/name for name in ('a','b')]
for root in roots:root.mkdir()
jobs={}; captured=[]; errors=[]; channels=[]; bridge=None; bridge_log=None
result={'status':'failed','binary':f.BIN,'root':str(f.ROOT)}
lock=threading.Lock()

def provider(self):
    try:
        request=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))
        with lock:
            captured.append(request)
            text='\n'.join(str(m.get('content','')) for m in request.get('messages',[]) if m.get('role')=='user')
            matches=[(key,job) for key,job in jobs.items() if key in text]
            assert matches, 'Unplanned provider request'
            key,job=matches[-1]
            job['requests'].append(request)
            first=len(job['requests'])==1
        if first:
            calls=[{'index':i,'id':str(uuid.uuid4()),'type':'function','function':{'name':name,'arguments':json.dumps(args)}} for i,(name,args) in enumerate(job['tools'])]
            delta={'tool_calls':calls};reason='tool_calls'
        else:
            delta={'content':'Synthetic permission workflow complete'};reason='stop'
        payload=''.join('data: '+json.dumps({'id':'scope-fixture','object':'chat.completion.chunk','choices':[{'index':0,'delta':d,'finish_reason':r}]})+'\n\n' for d,r in [(delta,None),({},reason)])+'data: [DONE]\n\n'
        body=payload.encode();self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    except Exception:errors.append(traceback.format_exc())
f.FixtureProvider.do_POST=provider

def uid():return str(uuid.uuid4())
def wait(check,label,seconds=120):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        value=check()
        if value:return value
        time.sleep(.05)
    f.capture_timeout_diagnostics(label)
    raise TimeoutError(label)
def connect():
    client=socket.socket(socket.AF_UNIX);client.settimeout(120);client.connect(str(f.sockpath));channel=(client,client.makefile('rb'));channels.append(channel);return channel
def close(channel):
    channel[1].close();channel[0].close();channels.remove(channel)
def rpc(channel,request_type,accept=None,**fields):
    f.counter+=1;identity=f.counter
    channel[0].sendall((json.dumps({'type':request_type,'id':identity,**fields})+'\n').encode())
    while True:
        line=channel[1].readline();assert line,'Control connection closed'
        event=json.loads(line);f.events.append(event)
        if event.get('id')!=identity or event.get('type')=='ack':continue
        if accept is not None and event.get('type') not in accept:continue
        return event

def ws(action,**fields):
    event=rpc(admin,'workspace',request={'action':action,**fields});assert event['type']=='workspace_response',event
    return event['response']
def ok(response,kind):
    assert response['kind']==kind,response
    return response['value']
def status():return ok(ws('status'),'status')
def permission(action,expected=None,**fields):
    response=ws('permissions',request={'action':action,**fields})
    if expected=='error':return ok(response,'error')
    nested=ok(response,'permissions')
    return ok(nested,expected) if expected else nested

def change(action,**fields):
    review=ok(ws('review',expected_revision=status()['revision'],change={'action':action,**fields}),'review')
    return ok(ws('apply',request=uid(),review=review['id']),'receipt')['targets'][0]
def grant(audience,target):
    review=permission('review','review',expected_revision=status()['revision'],change={'action':'issue','audience':audience,'target':target,'proposal':None})
    req=uid();receipt=permission('apply','mutation',request=req,review=review['id'])
    assert permission('apply','mutation',request=req,review=review['id'])==receipt
    return receipt['grant']
def revoke(value):
    review=permission('review','review',expected_revision=status()['revision'],change={'action':'revoke','grant':value['id']})
    return permission('apply','mutation',request=uid(),review=review['id'])
def saved(session):return json.loads((f.home/'sessions'/f'{session}.json').read_text())
def attach(session,root):
    channel=connect();assert rpc(channel,'subscribe',accept=('done','error'),working_dir=str(root),target_session_id=session,selfdev=False)['type']=='done';return channel

def turn(session,tools):
    key='SCOPE-JOB-'+uid();jobs[key]={'tools':tools,'requests':[]}
    response=rpc(admin,'primary_input',input={'id':uid(),'session':session,'delivery':'safe_boundary','content':key})
    assert response['type']=='primary_input_receipt',response
    wait(lambda:len(jobs[key]['requests'])>=2,'native-turn-'+key)
    def sealed():
        with sqlite3.connect(f.home/'execution/index.sqlite') as db:
            active=db.execute("SELECT count(*) FROM runs WHERE session_id=? AND state IN ('running','queued','stopping')",(session,)).fetchone()[0]
        return active==0
    wait(sealed,'tool-terminal-'+key)
    # Provider requests are recorded before the stream completes. Explicit state
    # settles the host turn before subsequent new-context controls.
    observer=attach(session,Path(saved(session)['working_dir']))
    wait(lambda:not rpc(observer,'state').get('is_processing',True),'turn-idle-'+key)
    close(observer)
    assert len(jobs[key]['requests'])==2,jobs[key]
    return jobs[key]['requests'][1]

def native(name,**args):return name,{'intent':'Verify owned scope fixture',**args}

try:
    f.start();admin=connect();ok(ws('initialize',request=uid()),'status')
    locations=[]
    for root in roots:
        target=change('register_location',name=root.name,path=str(root),registration={'kind':'standalone'})
        locations.append({'kind':'standalone','id':target['id']})
    launch=rpc(admin,'primary_launch',request={'request':uid(),'expected_revision':status()['revision'],'input':{'placement':{'kind':'existing','placement':locations[0]},'cwd':{'kind':'existing','path':str(roots[0])},'agent':None,'model':None,'selfdev':False}})
    assert launch['response']['status']=='launched',launch
    session=launch['response']['record']['session'];initial=saved(session)
    wait(lambda:saved(session).get('scope_notice'),'initial-scope-notice')
    assert not captured
    (roots[1]/'readable').write_text('read without write permission')
    read=turn(session,[native('read',file_path=str(roots[1]/'readable'))]);assert 'read without write permission' in json.dumps(read)
    turn(session,[native('write',file_path=str(roots[0]/'source.txt'),content='original')]);assert (roots[0]/'source.txt').read_text()=='original'
    turn(session,[native('write',file_path=str(roots[1]/'denied.txt'),content='denied')]);assert not (roots[1]/'denied.txt').exists()
    patch=f'*** Begin Patch\n*** Update File: {roots[0]/"source.txt"}\n@@\n-original\n+must stay unchanged\n*** Add File: {roots[1]/"late-denied.txt"}\n+denied\n*** End Patch'
    turn(session,[native('apply_patch',patch_text=patch)]);assert (roots[0]/'source.txt').read_text()=='original';assert not (roots[1]/'late-denied.txt').exists()
    proposed=permission('propose','mutation',session=session,request=uid(),target={'kind':'root','id':locations[1]['id']},reason='Synthetic cross-root task')['proposal']
    assert not any(root['location']['id']==locations[1]['id'] for root in permission('scope','scope',session=session)['roots'])
    review=permission('review','review',expected_revision=status()['revision'],change={'action':'issue','audience':{'kind':'session','id':session},'target':proposed['target'],'proposal':proposed['id']})
    direct=permission('apply','mutation',request=uid(),review=review['id'])['grant']
    revision=status()['revision'];before_calls=len(captured)
    wait(lambda:saved(session).get('scope_notice',{}).get('catalog_revision')==revision,'idle-grant-notice')
    assert len(captured)==before_calls
    turn(session,[native('write',file_path=str(roots[1]/'allowed.txt'),content='before')])
    turn(session,[native('edit',file_path=str(roots[1]/'allowed.txt'),old_string='before',new_string='after')])
    turn(session,[native('multiedit',file_path=str(roots[1]/'allowed.txt'),edits=[{'old_string':'after','new_string':'multi','replace_all':False}])])
    turn(session,[native('patch',patch_text=f'--- {roots[1]/"allowed.txt"}\n+++ {roots[1]/"allowed.txt"}\n@@ -1 +1 @@\n-multi\n+patched\n')])
    assert (roots[1]/'allowed.txt').read_text().strip()=='patched'
    # This source has direct grants. Legacy controls fail before creating a Session.
    source=attach(session,roots[0])
    count=len(list((f.home/'sessions').glob('*.json')))
    assert rpc(source,'split')['type']=='error'
    assert len(list((f.home/'sessions').glob('*.json')))==count
    carry=permission('review_carry','carry_review',session=session)
    split=rpc(source,'scoped_context',kind='split',grant_carry={'review':carry['id'],'carry':True})
    assert split['type']=='scoped_context_created',split
    child=split['session_id'];copies=permission('scope','scope',session=child)['grants'];copied=next(g for g in copies if g.get('copied_from')==direct['id']);assert copied['id']!=direct['id']
    revoke(direct)
    turn(session,[native('write',file_path=str(roots[1]/'revoked.txt'),content='denied')]);assert not (roots[1]/'revoked.txt').exists()
    turn(child,[native('write',file_path=str(roots[1]/'independent.txt'),content='copied')]);assert (roots[1]/'independent.txt').read_text()=='copied'
    # Real curated bridge and TypeScript SDK exercise reviewed Clear and explicit drop.
    api=ipc/'api.sock';bridge_log=(f.ROOT/'bridge.log').open('wb')
    bridge=subprocess.Popen([f.BIN,'--no-update','--no-selfdev','--socket',str(f.sockpath),'api-bridge','--api-socket',str(api)],env=f.env,stdout=bridge_log,stderr=bridge_log)
    wait(api.exists,'bridge-ready')
    module=(Path(__file__).resolve().parents[1]/'sdk/typescript/dist/index.js').as_uri()
    program=f'''import assert from 'node:assert/strict';import {{JcodeClient}} from {json.dumps(module)};
const c=await JcodeClient.connect({{socketPath:{json.dumps(str(api))},requestTimeoutMs:120000}});try{{await c.attachSession({json.dumps(child)});const review=await c.reviewGrantCarry({json.dumps(child)});assert.equal(review.direct_grants.length,1);const fresh=await c.createScopedContext({json.dumps(child)},'clear',{{review:review.id,carry:false}});console.log(JSON.stringify({{fresh}}));}}finally{{c.close();}}'''
    sdk=subprocess.Popen(['node','--input-type=module','-e',program],env=f.env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        stdout,stderr=sdk.communicate(timeout=30)
    except subprocess.TimeoutExpired:
        f.capture_timeout_diagnostics('scoped-context-over-30s')
        try: stdout,stderr=sdk.communicate(timeout=100)
        except subprocess.TimeoutExpired: sdk.kill();stdout,stderr=sdk.communicate();raise
    (f.ROOT/'sdk.stdout').write_text(stdout);(f.ROOT/'sdk.stderr').write_text(stderr);assert sdk.returncode==0,stderr
    fresh=json.loads(stdout)['fresh'];assert not permission('scope','scope',session=fresh)['grants']
    turn(fresh,[native('write',file_path=str(roots[1]/'dropped.txt'),content='denied')]);assert not (roots[1]/'dropped.txt').exists()
    assert saved(session)['system_prompt']['text']==initial['system_prompt']['text']
    assert saved(session)['messages'][:len(initial['messages'])]==initial['messages']
    assert not errors,errors
    before_restart=len(captured)
    for channel in list(channels):close(channel)
    if bridge and bridge.poll() is None:bridge.terminate();bridge.wait(timeout=20)
    f.reader.close();f.client.close();f.proc.terminate();f.proc.wait(timeout=30)
    f.start();admin=connect()
    assert not permission('scope','scope',session=session)['grants']
    assert any(g['id']==copied['id'] for g in permission('scope','scope',session=child)['grants'])
    assert not permission('scope','scope',session=fresh)['grants']
    assert len(captured)==before_restart
    turn(child,[native('write',file_path=str(roots[1]/'resumed-copy.txt'),content='resume preserves grant')])
    assert (roots[1]/'resumed-copy.txt').read_text()=='resume preserves grant'
    assert not errors,errors
    result.update(status='passed',restart_scope_preserved=True,no_effect_replay=True,session=session,provider_calls=len(captured),ordinary_read=True,all_native_mutators=True,all_patch_targets=True,proposal_not_authority=True,idle_no_inference=True,independent_carry=True,revocation=True,public_sdk_clear_drop=True,prefix_preserved=True)
except Exception:result['error']=traceback.format_exc()
finally:
    for channel in list(channels):close(channel)
    if bridge and bridge.poll() is None:
        bridge.terminate()
        try:bridge.wait(timeout=20)
        except subprocess.TimeoutExpired:bridge.kill();bridge.wait(timeout=10)
    if bridge_log:bridge_log.close()
    if f.reader:f.reader.close()
    if f.client:f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:f.proc.kill();f.proc.wait(timeout=10)
    f.http.shutdown();f.http.server_close();f.log.close()
    (f.ROOT/'scope-result.json').write_text(json.dumps(result,indent=2))
    (f.ROOT/'scope-events.json').write_text(json.dumps(f.events,indent=2))
    (f.ROOT/'scope-provider.json').write_text(json.dumps(captured,indent=2))
    (f.ROOT/'cleanup.json').write_text(json.dumps({'daemon_terminal':f.proc is None or f.proc.poll() is not None,'bridge_terminal':bridge is None or bridge.poll() is not None}))
    print(json.dumps(result));print('artifacts='+str(f.ROOT))
if result['status']!='passed':raise SystemExit(1)
