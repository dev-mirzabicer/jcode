#!/usr/bin/env python3
"""Joined primary/closeout retention through an owned real daemon, no paid model.
Run through run_isolated_test.py with --binary/--artifact-dir and a compiled
JCODE_WP07_SESSION_PROBE from crates/jcode-base/tests/closeout_retention_native.rs.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import hashlib, json, sqlite3, subprocess, time, uuid
from pathlib import Path
import test_instruction_manager as f

config=f.home/'config.toml'
config.write_text(config.read_text().replace('[features]','[features]\nmanaged_primary_launch=true',1))
checkout=f.ROOT/'owned-checkout'; repair=f.ROOT/'repair-cwd'
checkout.mkdir(); repair.mkdir()
(checkout/'linked.md').write_text('Unique linked closeout fixture information\n')
f.git(checkout,'init','-q');f.git(checkout,'commit','--allow-empty','-qm','fixture')
(f.ROOT/'wp07-retention-owner.json').write_text(json.dumps({'owner':str(uuid.uuid4())}))
actions=[]; intents=[]; session=None; attached=False; result=None; cleanup={'errors':[]}; terminal={'completed','cancelled','failed','interrupted'}

def complete(self):
    request=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))));f.posts.append(request)
    number=len(f.posts)
    def tool(index,name,arguments):
        return {'index':index,'id':f'retention-{number}-{index}','type':'function','function':{'name':name,'arguments':json.dumps(arguments)}}
    if number==1:
        delta={'tool_calls':[
            tool(0,'bash',{'command':'printf x >> effect-count; printf "retained tool output\\n"; pwd','intent':'Exercise owned retention fixture','notify':False,'wake':False}),
            tool(1,'side_panel',{'action':'load','page_id':'retained-link','file_path':str(checkout/'linked.md'),'focus':False,'intent':'Retain owned linked document'}),
        ]}; finish='tool_calls'
    elif number==3:
        delta={'tool_calls':[tool(0,'bash',{'command':'pwd > repaired-cwd; printf "repaired tool output\\n"','intent':'Verify explicitly repaired location','notify':False,'wake':False})]};finish='tool_calls'
    else: delta={'content':'Synthetic retention fixture complete'};finish='stop'
    chunks=[{'id':'fixture','object':'chat.completion.chunk','choices':[{'index':0,'delta':delta,'finish_reason':None}]},{'id':'fixture','object':'chat.completion.chunk','choices':[{'index':0,'delta':{},'finish_reason':finish}]}]
    body=(''.join('data: '+json.dumps(chunk)+'\n\n' for chunk in chunks)+'data: [DONE]\n\n').encode()
    self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
f.FixtureProvider.do_POST=complete

def uid():return str(uuid.uuid4())
def sha256(path):
    with Path(path).open('rb') as file:return hashlib.file_digest(file,'sha256').hexdigest()
def rpc(kind,**fields):
    f.counter+=1;rid=f.counter;f.send({'type':kind,'id':rid,**fields})
    return f.until(lambda e:e.get('id')==rid and e.get('type')!='ack' and (kind!='subscribe' or e.get('type') in ('done','error')))
def ws(action,kind,**fields):
    event=rpc('workspace',request={'action':action,**fields});assert event['type']=='workspace_response',event
    response=event['response'];assert response['kind']==kind,response;return response['value']
def change(action,**fields):
    review=ws('review','review',expected_revision=ws('status','status')['revision'],change={'action':action,**fields})
    receipt=ws('apply','receipt',request=uid(),review=review['id']);assert not receipt['issues'],receipt;return receipt['targets'][0]
def co(request):return ws('closeout','closeout',request=request)
def check_wait(check,label,seconds=180):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        value=check()
        if value:return value
        time.sleep(.05)
    f.capture_timeout_diagnostics(label);raise TimeoutError(label)
def runs():
    database=f.home/'execution/index.sqlite'
    if not database.exists():return []
    with sqlite3.connect(database.as_uri()+'?mode=ro',uri=True) as db:
        return db.execute('SELECT id,state,complete FROM runs WHERE session_id=? ORDER BY id',(session,)).fetchall()
def settled(expected_posts):
    rows=runs()
    return len(f.posts)==expected_posts and rows and all(row[1] in terminal and row[2] for row in rows) and not rpc('state')['is_processing']
def probe(label):
    env=f.env|{'JCODE_WP07_RETENTION_ROOT':str(f.ROOT),'JCODE_WP07_RETENTION_LABEL':label}
    run=subprocess.run([os.environ['JCODE_WP07_SESSION_PROBE'],'--ignored','--exact','closeout_native_capture_retained_session_and_outputs','--nocapture'],env=env,capture_output=True,text=True,timeout=60)
    (f.ROOT/f'probe-{label}.log').write_text(run.stdout+run.stderr);assert run.returncode==0,(run.stdout,run.stderr)
    return json.loads((f.ROOT/f'retention-{label}.json').read_text())
def preserved_after_closeout(before,closed):
    assert closed['outputs']==before['outputs'] and closed['side_panel']==before['side_panel']
    old=before['session'];new=closed['session']
    assert new['messages'][:len(old['messages'])]==old['messages']
    additions=new['messages'][len(old['messages']):]
    assert len(additions)<=1,additions
    if additions:
        assert additions[0]['id']==new['scope_notice']['message']
        assert additions[0]['role']=='user' and additions[0]['display_role']=='system'
    else:assert new.get('scope_notice')==old.get('scope_notice')
    # The existing scope owner checkpoints its appended notice atomically.
    # All other authoritative fields, not just a chosen transcript projection,
    # must remain identical. No prose parsing identifies this host control.
    expected=dict(old)
    for field in ('messages','scope_notice','persistence_identity','updated_at'):
        if field in new:expected[field]=new[field]
        else:expected.pop(field,None)
    assert expected==new,'Unrelated authoritative Session state changed'
    return len(additions)
def perform(record,action):
    request=uid();spec={'operation':record['operation'],'expected_revision':record['revision'],'action':action}
    intents.append({'request':request,'spec':spec});(f.ROOT/'action-intents.json').write_text(json.dumps(intents,indent=2))
    accepted=co({'action':'execute','request':request,'spec':spec});assert accepted['kind']=='action',accepted
    accepted=accepted['value'];actions.append(accepted)
    def done():
        status=co({'action':'execution','request':request,'control':{'action':'inspect','run_id':accepted['run_id']}})['value']['run']
        if status['state'] not in terminal:return False
        assert status['state']=='completed' and status['complete'],status
        value=co({'action':'inspect_action','request':request})['value'];assert value['issue'] is None,value
        return value['result']['value']
    return check_wait(done,'closeout-'+action['action'])

try:
    f.start();f.client.settimeout(120);ws('initialize','status',request=uid())
    project=change('create_project',name='retention');repository=change('create_repository',name='retention',remotes=[])
    change('associate_repository',project=project['id'],repository=repository['id'])
    location=change('register_location',name='retention-checkout',path=str(checkout),registration={'kind':'checkout','home':{'kind':'project','id':project['id']},'repository':repository['id']})
    destination=change('register_location',name='explicit-repair',path=str(repair),registration={'kind':'standalone'})
    launch=rpc('primary_launch',request={'request':uid(),'expected_revision':ws('status','status')['revision'],'input':{'placement':{'kind':'existing','placement':{'kind':'checkout','id':location['id']}},'cwd':{'kind':'existing','path':str(checkout)},'agent':None,'model':None,'selfdev':False}})
    assert launch['response']['status']=='launched',launch;session=launch['response']['record']['session']
    assert rpc('subscribe',target_session_id=session,working_dir=str(checkout),selfdev=False)['type']=='done'
    attached=True
    submitted=rpc('primary_input',input={'id':uid(),'session':session,'delivery':'safe_boundary','content':'INITIAL-RETENTION-FIXTURE','images':[]})
    assert submitted['type']=='primary_input_receipt',submitted
    check_wait(lambda:settled(2),'initial-primary-completion')
    assert (checkout/'effect-count').read_text()=='x'
    initial_runs=[row[0] for row in runs()];assert len(initial_runs)==2,initial_runs
    (f.ROOT/'retention-intent.json').write_text(json.dumps({'session':session,'runs':initial_runs}))
    before=probe('before');assert before['side_panel'],before
    record=co({'action':'begin','request':uid(),'expected_revision':ws('status','status')['revision'],'spec':{'location':location['id'],'expected_generation':1,'preservation_directory':None,'conditional_no_loss':False,'full_archive':True}})['value']
    record=perform(record,{'action':'refresh'});record=perform(record,{'action':'preserve'})
    review=perform(record,{'action':'review_removal'});assert not review['issues'],review
    record=co({'action':'inspect','operation':record['operation']})['value']
    record=perform(record,{'action':'approve_removal','review':review['id']})
    record=perform(record,{'action':'finish'});assert record['stage']=='closed' and not checkout.exists()
    closed=probe('closed');scope_notices=preserved_after_closeout(before,closed)
    assert len(f.posts)==2
    original={'id':uid(),'session':session,'delivery':'safe_boundary','content':'BLOCKED-ORIGINAL-INPUT','images':[]}
    blocked=rpc('primary_input',input=original)
    if blocked['type']=='primary_input_receipt':
        failed=check_wait(lambda:(receipt if (receipt:=rpc('primary_input_inspect',session=session,input=original['id']))['receipt']['state']=='failed' else False),'unavailable-input-failure')
        detail=rpc('primary_input_read',session=session,input=original['id'])
        assert detail['input']['content']==original['content'],detail
        rejection='failed_with_retained_original'
    else:
        assert blocked['type']=='error',blocked;rejection='rejected_before_acceptance'
    assert len(f.posts)==2 and not checkout.exists()
    unavailable=probe('unavailable')
    assert unavailable['session']['messages'][:len(before['session']['messages'])]==before['session']['messages']
    assert unavailable['outputs']==before['outputs'] and unavailable['side_panel']==before['side_panel']
    changed=rpc('primary_location',command={'action':'change','request':{'request':uid(),'session':session,'expected_session_revision':unavailable['session']['location']['revision'],'expected_catalog_revision':ws('status','status')['revision'],'placement':{'kind':'standalone','id':destination['id']},'cwd':str(repair)}})
    assert changed['response']['status']=='state' and changed['response']['record']['state']=='complete',changed
    repaired=probe('repaired')
    assert repaired['session']['working_dir']==str(repair.resolve())
    assert repaired['session']['location']['initial_cwd']==before['session']['location']['initial_cwd']
    assert repaired['session']['system_prompt']==before['session']['system_prompt']
    assert repaired['session']['messages'][:len(unavailable['session']['messages'])]==unavailable['session']['messages']
    assert repaired['outputs']==before['outputs'] and repaired['side_panel']==before['side_panel']
    assert len(f.posts)==2 and not checkout.exists(),'Location repair started inference or recreated a closed checkout'
    again=rpc('primary_input',input={'id':uid(),'session':session,'delivery':'safe_boundary','content':'AFTER-REPAIR','images':[]})
    assert again['type']=='primary_input_receipt',again
    check_wait(lambda:settled(4),'repaired-primary-completion')
    assert (repair/'repaired-cwd').read_text().strip()==str(repair.resolve())
    assert not checkout.exists()
    history=co({'action':'history','location':location['id']})['value'];assert history['record']['stage']=='closed'
    capture=next(path for path in Path(record['preservation_directory']).iterdir() if (path/'verified-restore/effect-count').is_file())
    assert (capture/'verified-restore/effect-count').read_text()=='x'
    assert (capture/'verified-restore/linked.md').read_text()=='Unique linked closeout fixture information\n'
    assert len(list((f.home/'sessions').glob('*.json')))==1,'Administration created a provisional Session'
    result={'session':session,'operation':record['operation'],'location':location['id'],'retained_runs':initial_runs,'prior_session_content_and_outputs_preserved':True,'nonwaking_scope_notices':scope_notices,'linked_metadata_equal':True,'explicit_cwd_repair':True,'input_refusal':rejection,'provider_requests':len(f.posts),'paid_requests':0,'fixture_managed_rollout':True,'history':history}
finally:
    try:
        if f.proc and f.proc.poll() is None:
            for intent in intents:
                inspected=co({'action':'inspect_action','request':intent['request']})
                if inspected['kind']!='action':continue
                action=inspected['value'];status=co({'action':'execution','request':action['request'],'control':{'action':'inspect','run_id':action['run_id']}})['value']['run']
                if status['state'] not in terminal:
                    co({'action':'execution','request':action['request'],'control':{'action':'stop','run_id':action['run_id']}})
                    check_wait(lambda:co({'action':'execution','request':action['request'],'control':{'action':'inspect','run_id':action['run_id']}})['value']['run']['state'] in terminal,'closeout-teardown',30)
            if session:
                if attached and rpc('state')['is_processing']:
                    f.counter+=1;f.send({'type':'cancel','id':f.counter})
                    check_wait(lambda:not rpc('state')['is_processing'],'primary-teardown',30)
                assert all(row[1] in terminal and row[2] for row in runs()),'Primary effects remain active; retain daemon for owned Stop/recovery'
        cleanup['owned_effects_terminal']=True
    except Exception as error:cleanup['errors'].append(repr(error))
    if f.reader:f.reader.close()
    if f.client:f.client.close()
    if not cleanup['errors'] and f.proc and f.proc.poll() is None:f.proc.terminate();f.proc.wait(timeout=30)
    cleanup['owned_daemon_terminal']=f.proc is None or f.proc.poll() is not None
    f.http.shutdown();f.log.close()
    (f.ROOT/'cleanup.json').write_text(json.dumps(cleanup,indent=2));(f.ROOT/'events.json').write_text(json.dumps(f.events,indent=2));(f.ROOT/'provider-posts.json').write_text(json.dumps(f.posts,indent=2))
    if result and not cleanup['errors']:
        result['artifacts']={str(Path(path).resolve()):sha256(path) for path in [f.BIN,os.environ['JCODE_WP07_SESSION_PROBE']]}
        (f.ROOT/'retention-result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
    print('artifacts='+str(f.ROOT));assert not cleanup['errors'],cleanup
