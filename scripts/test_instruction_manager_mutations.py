#!/usr/bin/env python3
"""Production manager mutation acceptance in private homes and local Git remotes.
Uses the read-only probe's daemon/protocol/debug helpers, never paid inference.
"""
import fcntl, json, os, pty, shlex, signal, struct, subprocess, sys, termios, threading, time
from pathlib import Path
# Assign a controlling PTY in a fresh interpreter, avoiding unsafe preexec_fn
# callbacks after the fixture HTTP thread has started.
if len(sys.argv)>1 and sys.argv[1]=='--pty-client':
    fcntl.ioctl(0,termios.TIOCSCTTY,0)
    os.execv(sys.argv[2],sys.argv[2:])
import test_instruction_manager as f

def bootstrap_response(self):
    f.posts.append(self.rfile.read(int(self.headers.get('Content-Length','0'))).decode())
    self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
    for delta,finish in [({'role':'assistant','content':'LOCAL FIXTURE RESPONSE'},None),({},'stop')]:
        event={'id':'fixture','object':'chat.completion.chunk','created':0,'model':'fixture','choices':[{'index':0,'delta':delta,'finish_reason':finish}]}
        self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
    self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
f.FixtureProvider.do_POST=bootstrap_response

clients=[]
server_debug=f.debug

def owned_client_debug(command):
    parts=command.split(':',2)
    if len(parts)!=3 or parts[0]!='tester' or not any(client[3]==parts[1] for client in clients):
        return server_debug(command)
    tid,operation=parts[1:]
    if operation.startswith('keys:'):
        keys(tid,operation.removeprefix('keys:'))
        return 'OK: physical PTY input'
    number=next(index for index,client in enumerate(clients) if client[3]==tid)
    request=f.ROOT/f'client-{number}.cmd';response=f.ROOT/f'client-{number}.response'
    file_command={'frame':'screen-json','frame-normalized':'screen-json-normalized'}.get(operation,operation)
    if response.exists():response.unlink()
    temporary=request.with_suffix('.new');temporary.write_text(file_command);os.replace(temporary,request)
    deadline=time.monotonic()+15
    while True:
        if response.exists():
            text=response.read_text()
            if text:
                response.unlink()
                with (f.ROOT/'debug.jsonl').open('a') as log:log.write(json.dumps({'command':command,'code':0,'stdout':text,'stderr':'','transport':'owned client debug file'})+'\n')
                return text
        if time.monotonic()>deadline:raise TimeoutError('Owned client did not answer '+command)
        time.sleep(.025)
f.debug=owned_client_debug
(f.ROOT/'mutation-probe.py').write_text(Path(__file__).read_text())

def complete_replay(session):
    return json.loads(subprocess.check_output([f.BIN,'--no-update','replay',session,'--export'],env=f.env,text=True))

def manage(request,expected=None):
    f.counter+=1; ident=f.counter
    f.send({'type':'manage_instructions','id':ident,'request':request})
    event=f.until(lambda event:event.get('id')==ident and event.get('type') in ('instruction_management','instruction_management_chunk','error'))
    if event['type']=='instruction_management_chunk':
        complete=bytearray();identity=event['chunk']['transfer'];total=event['chunk']['total_bytes']
        while True:
            chunk=event['chunk'];assert chunk['transfer']==identity and chunk['offset']==len(complete) and chunk['total_bytes']==total
            complete.extend(chunk['text'].encode());assert len(complete)<=total
            if len(complete)==total:break
            event=f.until(lambda event:event.get('id')==ident and event.get('type')=='instruction_management_chunk')
        event={'type':'instruction_management','reply':json.loads(complete)}
    assert event['type']=='instruction_management',event
    result=event['reply']['result']
    if expected: assert result['result']==expected,result
    return result.get('data',{}) if expected else result

def begin(identity,action,scope='global',origin=None):
    snapshot=f.open_snapshot()
    row=next(row for row in f.rows(snapshot) if (row['id']==identity or row['name']==identity) and row['scope']==scope and (origin is None or row['origin']==origin))
    return manage({'operation':'begin','snapshot':snapshot['snapshot'],'target':f.target(row),'action':action},'draft')

def update(draft,body,file=None):
    return manage({'operation':'update','draft':draft['id'],'generation':draft['generation'],'change':{'change':'body','file':file or draft['files'][0]['key'],'body':body}},'draft')

def save(draft,kind='saved'):
    review=manage({'operation':'review','draft':draft['id'],'generation':draft['generation']},'reviewed')
    assert not review['errors'],review
    result=manage({'operation':'save','draft':draft['id'],'generation':draft['generation']},kind)
    manage({'operation':'close'},'closed')
    return result

def operation(scope,action):
    plan=manage({'operation':'plan_repository','scope':scope,'action':action},'repository_plan')
    receipt=manage({'operation':'apply_repository','operation_id':plan['id']},'repository_receipt')
    assert receipt['completed'],receipt
    assert manage({'operation':'repository_receipt','operation_id':plan['id']},'repository_receipt')==receipt
    return receipt

def keys(tid,value):
    master=next(master for child,master,home,ident in clients if ident==tid)
    mapping={'f1':b'\x1bOP','f2':b'\x1bOQ','f3':b'\x1bOR','f4':b'\x1bOS','esc':b'\x1b','enter':b'\r','tab':b'\t','space':b' ','home':b'\x1b[H','end':b'\x1b[F','up':b'\x1b[A','down':b'\x1b[B','left':b'\x1b[D','right':b'\x1b[C','backspace':b'\x7f','ctrl+e':b'\x05','ctrl+u':b'\x15'}
    for key in value.split(','):
        data=mapping.get(key,key.encode())
        os.write(master,data)
        if key=='esc':time.sleep(.05)
    time.sleep(.04)

def ui(tid,predicate): return f.tester_state(tid,lambda state:predicate(state.get('editing',{})))

def spawn_client(session,cols,rows,reuse=None):
    number=len(clients);home=f.ROOT/f'client-{number if reuse is None else reuse}';home.mkdir(exist_ok=True);(home/'sessions').mkdir(exist_ok=True);(home/'sessions'/(session+'.json')).write_bytes((f.home/'sessions'/(session+'.json')).read_bytes());(home/'config.toml').write_bytes((f.home/'config.toml').read_bytes())
    command=f.ROOT/f'client-{number}.cmd';response=f.ROOT/f'client-{number}.response';output=f.ROOT/f'client-{number}.terminal'
    env={**f.env,'JCODE_HOME':str(home),'JCODE_RUNTIME_DIR':str(f.ROOT/f'client-runtime-{number if reuse is None else reuse}'),'JCODE_CLIENT_SELFDEV_MODE':'1','JCODE_DEBUG_CMD_PATH':str(command),'JCODE_DEBUG_RESPONSE_PATH':str(response),'VISUAL':'python3 '+shlex.quote(str(editor))}
    master,slave=pty.openpty();fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',rows,cols,0,0))
    child=subprocess.Popen([sys.executable,str(Path(__file__).resolve()),'--pty-client',f.BIN,'--no-update','--no-selfdev','--provider-profile','wp09-fixture','--model','fixture','--socket',str(f.sockpath),'--resume',session,'--debug-socket'],cwd=f.project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);os.close(slave)
    def drain():
        with output.open('wb') as stream:
            while True:
                try: data=os.read(master,65536)
                except OSError: break
                if not data: break
                stream.write(data);stream.flush()
    thread=threading.Thread(target=drain,daemon=True);thread.start()
    tid=f'wp10-client-{number}';record={'id':tid,'pid':child.pid,'binary':f.BIN,'cwd':str(f.project),'started_at':'2026-09-07T00:00:00Z','debug_cmd_path':str(command),'debug_response_path':str(response),'stdout_path':str(output),'stderr_path':str(output)}
    registry=f.home/'testers.json';entries=json.loads(registry.read_text()) if registry.exists() else [];entries.append(record);registry.write_text(json.dumps(entries))
    clients.append([child,master,home,tid]);f.debug(f'tester:{tid}:wait');keys(tid,'esc,esc');keys(tid,','.join('/instructions')+',enter');f.tester_state(tid,lambda state:state.get('visible') and state.get('rows_loaded',0)>0 and state.get('pending_id') is None)
    return tid

if __name__ == '__main__':
    try:
        f.start();f.session=f.subscribe();session=f.session;store=f.home/'instructions'
        f.counter+=1;bootstrap=f.counter
        f.send({'type':'message','id':bootstrap,'content':'Synthetic fixture bootstrap. No tools are needed.'})
        event=f.until(lambda event:event.get('id')==bootstrap and event.get('type') in ('done','error'))
        assert event['type']=='done',event
        assert len(f.posts)==1,f.posts
        bootstrap_requests=len(f.posts)
        # Session persistence is snapshot plus journal. Reconstruct through the real
        # server before taking a complete checkpoint baseline, not a partial JSON file.
        f.client.close();f.proc.terminate();f.proc.wait(timeout=10);f.start();f.subscribe(session)
        original_session=json.loads((f.home/'sessions'/(session+'.json')).read_text());(f.ROOT/'baseline-session.json').write_text(json.dumps(original_session,indent=2))
        original_replay=complete_replay(session);(f.ROOT/'baseline-replay.json').write_text(json.dumps(original_replay,indent=2))
        module=f.source(store,'modules','live-module','module','ORIGINAL')
        consumer=f.source(store,'modules','live-consumer','module','{{> live-module}}','template: handlebars\n')
        f.source(store,'modules','ui-module','module','UI ORIGINAL')
        large_body='合成🙂\n'*180000
        f.source(store,'modules','large-module','module',large_body)
        f.git(store,'add','modules');f.git(store,'commit','-m','synthetic manager fixture')
        large=begin('large-module',{'action':'edit'});assert large['files'][0]['body']==large_body;save(large)
        draft=begin('live-module',{'action':'edit'});draft=update(draft,'API EDIT 合成')
        assert module.read_text().endswith('ORIGINAL');saved=save(draft)
        assert module.read_text().endswith('API EDIT 合成');assert f.git(store,'show','--format=','--name-only',saved['commit'])=='modules/live-module.md'
        noop=save(begin('live-module',{'action':'edit'}));assert noop['no_change']
        renamed=save(begin('live-module',{'action':'rename','id':'live-renamed'}));assert not module.exists();assert 'live-renamed' in consumer.read_text()
        revision=renamed['commit'];deleted=begin('live-renamed',{'action':'delete'});deleted=update(deleted,'NO REFERENCE','modules/live-consumer.md');save(deleted)
        snapshot=f.open_snapshot();restored=manage({'operation':'begin','snapshot':snapshot['snapshot'],'target':{'target':'repository','key':'global'},'action':{'action':'restore_path','revision':revision,'path':'modules/live-renamed.md'}},'draft');save(restored)
        assert (store/'modules/live-renamed.md').read_text().endswith('API EDIT 合成')
        operation('project',{'action':'standalone','path':'.jcode/instructions'})
        external=f.project/'.jcode/skills/live-skill';external.mkdir(parents=True);(external/'SKILL.md').write_text('---\nname: live-skill\ndescription: synthetic\n---\nSKILL BODY {{literal}}');(external/'binary.dat').write_bytes(b'\xff\x00\xfe')
        copied=begin('live-skill',{'action':'copy_skill','scope':'global','destination_id':None},'project','external');save(copied);assert (store/'skills/live-skill/binary.dat').read_bytes()==b'\xff\x00\xfe'
        (f.project/'.jcode/prompt-overlay.md').write_text('EXACT LEGACY\r\n合成')
        imported=begin('prompt-overlay.md',{'action':'import_legacy'},'project','legacy');save(imported)
        agents=f.project/'AGENTS.md';agents.write_text('ECOSYSTEM ORIGINAL');ec=begin('AGENTS.md',{'action':'edit'},'project');assert ec['working_file_only'];ec=update(ec,'ECOSYSTEM NEW');save(ec,'file_saved');assert agents.read_text()=='ECOSYSTEM NEW'
        stale=begin('live-renamed',{'action':'edit'});stale=update(stale,'PROPOSED AFTER STALE');(store/'modules/live-renamed.md').write_text('---\nid: live-renamed\nkind: module\n---\nEXTERNAL CHANGE')
        rejected=manage({'operation':'review','draft':stale['id'],'generation':stale['generation']});assert rejected['result']=='failed'
        comparison=manage({'operation':'compare_draft','draft':stale['id'],'generation':stale['generation']},'draft_conflict')
        reconciled=manage({'operation':'reconcile_draft','draft':stale['id'],'generation':stale['generation'],'comparison':comparison['comparison'],'use_working_content':False},'draft');assert reconciled['id']!=stale['id'];save(reconciled)
        retained=manage({'operation':'recoveries'},'recoveries');assert any(item['id']==stale['id'] for item in retained['drafts'])
        print('JCODE_PROGRESS '+json.dumps({'message':'Production mutation, package, import, ecosystem and stale-source APIs passed','current':1,'total':3}),flush=True)
        desired=f.ROOT/'editor-content';desired.write_text('UI EDIT 合成');mode=f.ROOT/'editor-mode';mode.write_text('success');editor=f.ROOT/'editor with spaces.py'
        editor.write_text('import sys,termios,json\nfrom pathlib import Path\np=Path(sys.argv[-1])\nassert termios.tcgetattr(0)[3]&termios.ICANON\np.write_text(Path('+repr(str(desired))+').read_text())\nsys.exit(3 if Path('+repr(str(mode))+').read_text()=="fail" else 0)\n')
        tid=spawn_client(session,150,40)
        def resize_client(cols, rows):
            child,master,_,_=clients[-1]
            fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',rows,cols,0,0))
            os.kill(child.pid,signal.SIGWINCH)
        for cols,rows in [(150,40),(80,24),(60,24),(24,10)]:
            resize_client(cols,rows)
            keys(tid,'c,/,u,i,-,m,o,d,u,l,e,enter');f.tester_state(tid,lambda state:state.get('rows_loaded')==1 and state.get('pending_id') is None);mode.write_text('fail');keys(tid,'ctrl+e');ui(tid,lambda state:state.get('failed') and state.get('pending') is None)
            mode.write_text('success');desired.write_text(f'UI EDIT {cols} 合成');keys(tid,'b');ui(tid,lambda state:not state.get('failed') and state.get('generation',0)>=2 and state.get('pending') is None)
            ui(tid,lambda state:state.get('reviewed') and state.get('pending') is None)
            view=f.frame(tid,'UI EDIT',motion='end');assert view['terminal_size']==[cols,rows];(f.ROOT/f'edit-review-{cols}.json').write_text(json.dumps(view,indent=2))
            if cols==150:
                button=next(item['rect'] for item in view['layout']['widget_placements'] if item['kind']=='instruction-action-S')
                f.debug(f"tester:{tid}:mouse:click:{button['x']},{button['y']}")
            else:keys(tid,'s')
            ui(tid,lambda state:state.get('pending') is None and not state.get('failed') and not state.get('reviewed'))
            deadline=time.monotonic()+20
            while not (store/'modules/ui-module.md').read_text().endswith(f'UI EDIT {cols} 合成'):
                if time.monotonic()>deadline:raise TimeoutError('Reviewed Save did not commit')
                time.sleep(.1)
            keys(tid,'esc');ui(tid,lambda state:not state.get('visible'))
            if cols==150:
                f.tester_state(tid,lambda state:state.get('pending_id') is None and state.get('rows_loaded',0)>0)
                keys(tid,'space');keys(tid,','.join('space' if char==' ' else char for char in 'global repository'));keys(tid,'enter')
                f.tester_state(tid,lambda state:state.get('menu') is None and state.get('editing',{}).get('visible') and state.get('editing',{}).get('pending') is None)
                f.frame(tid,'Global repository controls',motion='home')
                keys(tid,'enter');keys(tid,','.join('space' if char==' ' else char for char in 'create working branch'));keys(tid,'enter')
                f.frame(tid,'New local branch',motion='home')
                keys(tid,'tab');keys(tid,','.join('reviewed-ui-branch'));keys(tid,'tab,tab,enter')
                plan_frame=f.frame(tid,'Create and select branch reviewed-ui-branch',motion='home')
                (f.ROOT/'repository-plan.json').write_text(json.dumps(plan_frame,indent=2))
                assert f.git(store,'branch','--show-current')=='main'
                keys(tid,'n');assert f.git(store,'branch','--show-current')=='main'
                keys(tid,'enter');f.frame(tid,'Create and select branch reviewed-ui-branch',motion='home');keys(tid,'y')
                deadline=time.monotonic()+20
                while f.git(store,'branch','--show-current')!='reviewed-ui-branch':
                    if time.monotonic()>deadline:raise TimeoutError('Confirmed repository action did not select its branch')
                    time.sleep(.1)
                f.frame(tid,'Repository action completed.',motion='home')
                ui(tid,lambda state:state.get('pending') is None and not state.get('failed'))
                keys(tid,'esc');ui(tid,lambda state:not state.get('visible'))
        print('JCODE_PROGRESS '+json.dumps({'message':'Separate-home physical editor failure/retry/review/Save passed at all four sizes','current':2,'total':3}),flush=True)
        # Recover actual unsent form state after the client process exits. Its PTY
        # is owned by this probe, independent of the server process lifetime.
        recovery_client=len(clients)-1
        resize_client(80,24)
        keys(tid,'c,/,u,i,-,m,o,d,u,l,e,enter');f.tester_state(tid,lambda state:state.get('rows_loaded')==1 and state.get('pending_id') is None);keys(tid,'ctrl+e');opened=ui(tid,lambda state:state.get('draft') and state.get('reviewed') and state.get('pending') is None)
        retained_id=opened['editing']['draft'];keys(tid,'m');keys(tid,'r,e,t,a,i,n,e,d,-,n,a,m,e')
        capsule_root=f.ROOT/f'client-runtime-{recovery_client}'/'durable-state/instruction-editor/client-recovery'
        deadline=time.monotonic()+20
        while True:
            records=[json.loads(path.read_text()) for path in capsule_root.rglob('*.json')]
            if any(any(field.get('value')=='retained-name' for field in (record.get('form') or {}).get('fields',[])) for record in records):break
            if time.monotonic()>deadline:raise TimeoutError('Unsent form did not reach private recovery storage')
            time.sleep(.1)
        child,master,_,_=clients[-1];child.terminate();child.wait(timeout=10);os.close(master);clients[-1][1]=None
        tid=spawn_client(session,80,24,reuse=recovery_client);keys(tid,'space');keys(tid,'l,o,c,a,l,space,u,n,s,e,n,t,enter');f.tester_state(tid,lambda state:state.get('menu')=='Local unsent changes');keys(tid,'enter')
        ui(tid,lambda state:state.get('draft')==retained_id and state.get('pending') is None)
        recovered=f.frame(tid,'retained-name',motion='home');(f.ROOT/'recovered-local-form.json').write_text(json.dumps(recovered,indent=2))
        assert 'name: retained-name' not in (store/'modules/ui-module.md').read_text()
        keys(tid,'esc,esc');ui(tid,lambda state:not state.get('visible'))
        child,master,_,_=clients[-1];child.terminate();child.wait(timeout=10);os.close(master);clients[-1][1]=None
        # A server restart preserves exact session instructions and server-side draft identity.
        draft=begin('live-renamed',{'action':'edit'});draft=update(draft,'RETAINED THROUGH RESTART');manage({'operation':'close'},'closed')
        f.client.close();f.proc.terminate();f.proc.wait(timeout=10);f.start();f.subscribe(session)
        resumed=manage({'operation':'resume','scope':'global','draft':draft['id']},'draft');assert resumed['generation']==draft['generation'];assert resumed['files'][0]['body']=='RETAINED THROUGH RESTART';save(resumed)
        current_session=json.loads((f.home/'sessions'/(session+'.json')).read_text())
        for key in ['system_prompt','active_skill','model','reasoning_effort','route_api_method']:assert original_session.get(key)==current_session.get(key),key
        current_replay=complete_replay(session);(f.ROOT/'final-replay.json').write_text(json.dumps(current_replay,indent=2));assert original_replay==current_replay,'complete journal-aware replay changed'
        assert len(f.posts)==bootstrap_requests,f.posts
        evidence={'binary':subprocess.check_output([f.BIN,'--version'],text=True).strip(),'production_apis':True,'separate_client_home':True,'editor_failure_and_retry':True,'physical_mouse_save':True,'physical_repository_review_cancel_apply':True,'physical_sizes':[[150,40],[80,24],[60,24],[24,10]],'same_client_physical_resize':True,'server_restart_draft_recovery':True,'physical_unsent_form_recovery':True,'session_instructions_unchanged':True,'complete_journal_aware_replay_unchanged':True,'manager_inference_requests':len(f.posts)-bootstrap_requests,'localhost_bootstrap_requests':bootstrap_requests,'artifact':str(f.ROOT)}
        (f.ROOT/'mutation-evidence.json').write_text(json.dumps(evidence,indent=2));print(json.dumps(evidence),flush=True)
    finally:
        for child,master,_,_ in clients:
            if child.poll() is None:
                child.terminate()
                try:child.wait(timeout=10)
                except subprocess.TimeoutExpired:child.kill();child.wait()
            try:
                if master is not None:os.close(master)
            except OSError:pass
        if f.client:f.client.close()
        if f.proc and f.proc.poll() is None:
            f.proc.terminate()
            try:f.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:f.proc.kill();f.proc.wait()
        f.http.shutdown();f.log.close()
        (f.ROOT/'events.json').write_text(json.dumps(f.events));(f.ROOT/'provider-posts.json').write_text(json.dumps(f.posts))
