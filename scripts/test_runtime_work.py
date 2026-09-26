#!/usr/bin/env python3
"""Combined reviewed runtime + actual native work. Owned fixture only, no paid calls.
Run via run_isolated_test.py with --binary and --artifact-dir.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Use scripts/run_isolated_test.py')
import fcntl, hashlib, json, shlex, signal, socket, sqlite3, subprocess, sys, tempfile, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f

ipc=Path(tempfile.mkdtemp(prefix='rw-',dir=os.environ['JCODE_RUNTIME_DIR']))
old=str(f.sockpath); f.sockpath=ipc/'runtime.sock'
f.args=[str(f.sockpath) if arg==old else arg for arg in f.args]
f.env['JCODE_SOCKET']=str(f.sockpath)
prefix=f.args[:f.args.index('serve')]
(f.ROOT/'owned-runtime-work.json').write_text(json.dumps({'binary':f.BIN,'namespace':str(f.sockpath)}))
commands=[]; clients=[]; runs=[]; errors=[]; cleanup=[]; outcomes={}
testers=[]; frames=[]; physical_tui=os.environ.get("JCODE_WP08_PHYSICAL_TUI")=="1"
fixture=f.ROOT/'native.py'
fixture.write_text('''from pathlib import Path
import os,sys,time
root=Path(sys.argv[1]); mode=sys.argv[2]
(root/(mode+'-cwd')).write_text(os.getcwd())
with (root/(mode+'-effects')).open('a') as f: f.write('x')
print('PREFIX_'+mode,flush=True)
(root/(mode+'-ready')).write_text('ready')
deadline=time.monotonic()+180
while not (root/(mode+'-release')).exists():
 if time.monotonic()>deadline: raise RuntimeError('owned fixture deadline')
 time.sleep(.05)
print('TAIL_'+mode,flush=True)
''')

def provider(self):
    body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
    f.posts.append(body)
    self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
    for delta,reason in [({'content':'DEFERRED_INPUT_DELIVERED'},None),({},'stop')]:
        self.wfile.write(('data: '+json.dumps({'id':'fixture','choices':[{'index':0,'delta':delta,'finish_reason':reason}]})+'\n\n').encode())
    self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
f.FixtureProvider.do_POST=provider

def progress(stage):
    (f.ROOT/'progress.json').write_text(json.dumps({'stage':stage,'time':time.time()}))
    print(json.dumps({'stage':stage,'root':str(f.ROOT)}),flush=True)
def deadline(_signal,_frame):
    raise TimeoutError('Owned fixture watchdog expired')
signal.signal(signal.SIGALRM,deadline)
signal.alarm(480)

def wait(predicate,label,seconds=40):
    deadline=time.monotonic()+seconds
    while not predicate():
        if time.monotonic()>deadline: raise TimeoutError(label)
        time.sleep(.05)

def cli(*args,success=True):
    value=subprocess.run(prefix+['runtime',*args,'--json'],env=f.env,cwd=f.project,capture_output=True,text=True,timeout=65)
    commands.append({'args':args,'exit':value.returncode,'stdout':value.stdout,'stderr':value.stderr})
    (f.ROOT/'cli.json').write_text(json.dumps(commands,indent=2))
    if success: assert value.returncode==0,commands[-1]
    else: assert value.returncode!=0,commands[-1];return value
    return json.loads(value.stdout)

def current(): return cli('status')['response']['value']
def inspect(op): return cli('inspect',op)['response']['value']
def begin(strategy,tasks,timeout=10):
    review=cli('stop','--strategy',strategy,'--tasks',tasks,'--quiescence-seconds',str(timeout))
    return cli('confirm',review['response']['value']['id'],'--request',review['confirm_request'])['response']['value']
def change(op,strategy,tasks):
    latest=inspect(op['id'])
    review=cli('change',op['id'],'--revision',str(latest['revision']),'--strategy',strategy,'--tasks',tasks,'--quiescence-seconds','15')
    return cli('confirm',review['response']['value']['id'],'--request',review['confirm_request'])['response']['value']
def stopped(op):
    value=cli('wait',op['id'],'--timeout-seconds','35')
    assert value['response']['value']['phase']=='stopped' and value['coordinator_owned'] is False,value
    assert not f.sockpath.exists()
    return value['response']['value']

class Client:
    def __init__(self):
        self.sock=socket.socket(socket.AF_UNIX);self.sock.settimeout(45);self.sock.connect(str(f.sockpath))
        self.stream=self.sock.makefile('rwb',buffering=0);self.n=0;self.events=[];clients.append(self)
    def send(self,kind,**fields):
        self.n+=1;self.stream.write((json.dumps({'type':kind,'id':self.n,**fields})+'\n').encode());return self.n
    def until(self,predicate):
        while True:
            data=self.stream.readline()
            if not data: raise EOFError('owned runtime connection ended')
            event=json.loads(data);self.events.append(event)
            if event.get('type')=='error': raise AssertionError(event)
            if predicate(event): return event
    def rpc(self,kind,**fields):
        ident=self.send(kind,**fields);return self.until(lambda e:e.get('id')==ident and e['type']!='ack')
    def subscribe(self,session=None):
        ident=self.send('subscribe',working_dir=str(f.project),selfdev=False,**({'target_session_id':session} if session else {'agent':'global:jcode'}))
        self.until(lambda e:e.get('id')==ident and e['type']=='done')
        return [e['session_id'] for e in self.events if e['type'] in ('session','history')][-1]
    def execution(self,action,run,**fields):
        event=self.rpc('execution',request={'action':action,'run_id':run,**fields})
        assert event['type']=='execution_response',event
        return event['response']
    def close(self):
        if self in clients: clients.remove(self)
        (f.ROOT/f'events-{id(self)}.json').write_text(json.dumps(self.events,indent=2))
        self.stream.close();self.sock.close()

def tester_command(tid, text):
    row=next(row for row in json.loads((f.home/'testers.json').read_text()) if row['id']==tid)
    target=Path(row['debug_cmd_path']); reply=Path(row['debug_response_path'])
    if reply.exists(): reply.unlink()
    temporary=target.with_suffix('.next'); temporary.write_text(text); os.replace(temporary,target)
    wait(lambda:reply.exists() and reply.stat().st_size>0,'tester '+text,15)
    value=reply.read_text(); reply.unlink()
    with (f.ROOT/'tui-commands.jsonl').open('a') as out:out.write(json.dumps({'tester':tid,'command':text,'response':value})+'\n')
    return value

def tester_frame(tid,name,expected=None):
    captured=[]
    def check():
        raw=tester_command(tid,'screen-json')
        if not raw.lstrip().startswith('{'):return False
        value=json.loads(raw)
        if expected:
            # Frame metadata clips each content_preview to 200 characters.
            # Inspect this same TUI's complete display history for exact output.
            history=json.loads(tester_command(tid,'history'))
            if not any(expected in row.get('content','') for row in history):return False
            if not any(row.get('role')=='system' for row in value.get('rendered_text',{}).get('recent_messages',[])):return False
            (f.ROOT/(name+'-history.json')).write_text(json.dumps(history,indent=2))
        assert not value.get('anomalies'),value.get('anomalies')
        captured.append(value);return True
    wait(check,'physical frame '+name,20)
    (f.ROOT/(name+'.json')).write_text(json.dumps(captured[-1],indent=2));frames.append(name)

def stop_testers():
    for tid in list(testers):
        f.debug('tester:'+tid+':stop');testers.remove(tid)

def tui_shell(command,mode):
    cols=120 if mode in ('finish-cancel','background-stop') else 80
    wrapper=f.ROOT/(mode+'-tui.sh')
    args=[f.BIN,'--no-update','--no-selfdev','--provider-profile','wp09-fixture','--model','fixture','--socket',str(f.sockpath),'--resume',session]
    wrapper.write_text('#!/bin/sh\nexec '+' '.join(map(shlex.quote,args)));wrapper.chmod(0o700)
    f.debug('tester:spawn '+json.dumps({'cwd':str(f.project),'binary':str(wrapper),'cols':cols,'rows':32 if cols==120 else 24}))
    tid=json.loads((f.home/'testers.json').read_text())[-1]['id'];testers.append(tid)
    wait(lambda:session in tester_command(tid,'startup-context-state'),'tester attached',25)
    tester_command(tid,'keys:esc,esc')
    tester_command(tid,'set_input:!'+command);tester_command(tid,'keys:enter')
    wait(lambda:(f.ROOT/(mode+'-ready')).exists(),'physical shell started')
    tester_frame(tid,'tui-running-'+mode)
    if mode!='finish-cancel':stop_testers()


def record(run):
    with sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro',uri=True) as db:
        return db.execute('SELECT state,background,owner FROM runs WHERE id=?',(run,)).fetchone()
def shell(client,mode):
    with sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro',uri=True) as db:
        before={row[0] for row in db.execute('SELECT id FROM runs')}
    command=f'{shlex.quote(sys.executable)} {shlex.quote(str(fixture))} {shlex.quote(str(f.ROOT))} {mode}'
    if physical_tui:tui_shell(command,mode)
    else:client.send('input_shell',command=command)
    wait(lambda:(f.ROOT/(mode+'-ready')).exists(),mode+' ready')
    with sqlite3.connect(f'file:{f.home}/execution/index.sqlite?mode=ro',uri=True) as db:
        after={row[0] for row in db.execute("SELECT id FROM runs WHERE tool='input_shell'")}
    assert len(after-before)==1,after-before
    run=(after-before).pop();runs.append({'id':run,'mode':mode})
    (f.ROOT/'runs.json').write_text(json.dumps(runs,indent=2))
    assert (f.ROOT/(mode+'-effects')).read_text()=='x'
    assert (f.ROOT/(mode+'-cwd')).read_text()==str(f.project.resolve())
    return run

def output(client,run):
    reply=client.execution('read',run,content='output',output_size=10000)
    assert reply['kind']=='content',reply
    return reply['page']['output']

def resume(session):
    for c in list(clients):c.close()
    cli('start');c=Client();assert c.subscribe(session)==session;return c

try:
    progress('start')
    f.start();f.reader.close();f.client.close();f.reader=f.client=None
    client=Client();session=client.subscribe();outcomes['session']=session
    progress('finish-cancel')
    run=shell(client,'finish-cancel')
    op=begin('finish-current','stop',1)
    time.sleep(1.25)
    assert inspect(op['id'])['phase']=='waiting_for_current','Natural Finish must not use the quiescence timeout'
    source=Client();ident=str(uuid.uuid4())
    reply=source.rpc('primary_input',input={'id':ident,'session':session,'content':'deferred fixture input','delivery':'safe_boundary','urgent':True,'origin':{'kind':'human'}})
    assert reply['receipt']['state']=='accepted',reply
    source.close();assert not f.posts
    latest=inspect(op['id']);cancelled=cli('cancel',op['id'],'--revision',str(latest['revision']))
    assert cancelled['response']['value']['phase']=='cancelled'
    wait(lambda:len(f.posts)==1,'deferred input resumed')
    wait(lambda:client.rpc('primary_input_inspect',session=session,input=ident)['receipt']['state']=='committed','input committed')
    (f.ROOT/'finish-cancel-release').write_text('release')
    wait(lambda:record(run)[0]=='completed','finish command completed')
    assert 'TAIL_finish-cancel' in output(client,run)
    if physical_tui:
        tester_frame(testers[-1],'tui-completed-wide','TAIL_finish-cancel')
        stop_testers()
    outcomes['finish_wait_cancel_input']=True

    progress('foreground-keep')
    run=shell(client,'foreground-keep')
    op=begin('finish-current','stop')
    replacement=change(op,'interrupt','keep-supported')
    assert inspect(op['id'])['phase']=='superseded'
    terminal=stopped(replacement)
    assert any(work['id']==run for work in terminal['preserved']),terminal
    assert record(run)[0:2]==('running',1),record(run)
    f.proc.wait(timeout=20);assert f.proc.returncode==0
    assert cli('status')['response']['value']['desired_stopped'] and len(f.posts)==1
    client=resume(session)
    assert client.execution('inspect',run)['run']['state']=='running'
    assert 'PREFIX_foreground-keep' in output(client,run)
    (f.ROOT/'foreground-keep-release').write_text('release')
    wait(lambda:record(run)[0]=='completed','preserved foreground completion')
    assert 'TAIL_foreground-keep' in output(client,run)
    outcomes['actual_daemon_exit_foreground_survival']=True

    progress('background-stop')
    run=shell(client,'background-stop')
    assert client.execution('background',run)['accepted']
    stopped(begin('interrupt','stop'))
    assert record(run)[0]=='cancelled',record(run)
    client=resume(session)
    assert 'PREFIX_background-stop' in output(client,run) and 'TAIL_background-stop' not in output(client,run)
    outcomes['actual_background_stop']=True

    progress('background-keep')
    run=shell(client,'background-keep')
    assert client.execution('background',run)['accepted']
    terminal=stopped(begin('finish-current','keep-supported'))
    assert any(work['id']==run for work in terminal['preserved']),terminal
    assert record(run)[0:2]==('running',1)
    client=resume(session)
    (f.ROOT/'background-keep-release').write_text('release')
    wait(lambda:record(run)[0]=='completed','preserved background completion')
    assert 'TAIL_background-keep' in output(client,run)
    outcomes['actual_daemon_exit_background_survival']=True
    assert len(f.posts)==1,f.posts
    for item in runs: assert (f.ROOT/(item['mode']+'-effects')).read_text()=='x'
except Exception as error:
    errors.append(traceback.format_exc())
finally:
    signal.alarm(100)
    progress('cleanup')
    for item in runs: (f.ROOT/(item['mode']+'-release')).write_text('cleanup release')
    try:
        if testers:
            if not f.sockpath.exists():cli('start')
            stop_testers()
        for c in list(clients):c.close()
        if f.sockpath.exists():
            peer=Client()
            def control(request):
                event=peer.rpc('runtime_control',request=request)
                assert event['type']=='runtime_response',event
                value=event['response'];assert value['kind']!='error',value
                return value['value']
            status=control({'action':'status'});op=status['operation']
            options={'strategy':'interrupt','independent':'stop','quiescence_timeout_seconds':20}
            if op and op['phase'] in ('waiting_for_current','blocked'):
                review=control({'action':'review_change','operation':op['id'],'expected_revision':op['revision'],'options':options})
            elif not status['desired_stopped'] or not op or op['review']['runtime'] != status['runtime'] or op['phase'] in ('cancelled','superseded','interrupted'):
                review=control({'action':'review','options':options})
            else: review=None
            if review:
                try:op=control({'action':'begin','request':str(uuid.uuid4()),'review':review['id']})
                except EOFError: pass  # Verify actual exit/leases below, not the missing reply.
            peer.close()
            wait(lambda:not f.sockpath.exists(),'cleanup runtime exit',35)
        offline=cli('status')
        assert not offline['live_response'] and offline['coordinator_owned'] is False,offline
        for item in runs: wait(lambda:record(item['id'])[0] in ('completed','cancelled','failed','interrupted'),'cleanup terminal')
        leases=list((f.home/'execution/runtimes').glob('*.lease'));assert leases
        def all_released():
            for lease in leases:
                with lease.open('r+') as stream:
                    try:fcntl.flock(stream,fcntl.LOCK_EX|fcntl.LOCK_NB)
                    except BlockingIOError:return False
            return True
        wait(all_released,'all worker/runtime leases released')
        cleanup.append({'all_terminal':True,'released_leases':len(leases)})
    except Exception as error: cleanup.append({'error':repr(error)})
    if f.proc is not None and f.proc.poll() is None:
        try:f.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:cleanup.append({'error':'Original fixture daemon still alive'})
    f.http.shutdown();f.log.close()
    signal.alarm(0)
    result={'root':str(f.ROOT),'binary':f.BIN,'errors':errors,'cleanup':cleanup,'outcomes':outcomes,'physical_tui':physical_tui,'frames':frames,'remaining_testers':testers,'runs':runs,'provider_requests':len(f.posts),'sha256':hashlib.file_digest(open(f.BIN,'rb'),'sha256').hexdigest()}
    (f.ROOT/'result.json').write_text(json.dumps(result,indent=2));(f.ROOT/'provider-posts.json').write_text(json.dumps(f.posts,indent=2))
    print(json.dumps(result))
if errors or any('error' in item for item in cleanup):raise SystemExit(1)
