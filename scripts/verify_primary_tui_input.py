#!/usr/bin/env python3
"""Actual TUI input replay across lost receipts and client replacement.

The proxy drops only owned fixture connections. Inference is a localhost script,
not a behavioral model evaluation. Run through run_isolated_test.py.
"""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import base64, json, shlex, socket, subprocess, tempfile, threading, time, traceback, uuid
from pathlib import Path
import test_instruction_manager as f
ipc=Path(tempfile.mkdtemp(prefix='pi-',dir=os.environ['JCODE_RUNTIME_DIR']))
old=str(f.sockpath)
f.sockpath=ipc/'daemon.sock'
f.args=[str(f.sockpath) if item==old else item for item in f.args]
f.env['JCODE_SOCKET']=str(f.sockpath)
proxy_path=ipc/'client.sock'
config=f.home/'config.toml'
config.write_text(config.read_text().replace('[features]','[features]\nmanaged_primary_launch=true',1))
first_release=threading.Event(); soft_release=threading.Event()
first_entered=threading.Event(); soft_entered=threading.Event()
closed=threading.Event(); dropped=threading.Event()
lock=threading.Lock(); captures=[]; submissions=[]; errors=[]; testers=[]; frames=[]
proxy_state={'drop':'next_turn','blocked':False}
connections=[]

def wait(predicate, label, seconds=90):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        if predicate(): return
        time.sleep(.03)
    raise TimeoutError(label)

def response(self):
    body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))
    with lock:
        captures.append(body); number=len(captures)
    self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
    def event(delta,stop=None):
        value={'id':'fixture','object':'chat.completion.chunk','choices':[{'index':0,'delta':delta,'finish_reason':stop}]}
        self.wfile.write(('data: '+json.dumps(value)+'\n\n').encode()); self.wfile.flush()
    try:
        if number in (1,3):
            entered,release=(first_entered,first_release) if number==1 else (soft_entered,soft_release)
            entered.set(); assert release.wait(180),'owned provider gate not released'
        if number in (1,4):
            name='first' if number==1 else 'soft'
            command=f"printf x >> {shlex.quote(str(f.project/(name+'-effect')))}; pwd"
            args=json.dumps({'command':command,'intent':'Verify one owned TUI input effect','run_in_background':False,'notify':False,'wake':False})
            event({'role':'assistant','tool_calls':[{'index':0,'id':name+'-tool','type':'function','function':{'name':'bash','arguments':args}}]},'tool_calls')
        elif number==3:
            event({'content':'Held turn finished. '},'stop')
        elif number in (2,5):
            event({'content':'DURABLE TUI RESULT '+str(number)},'stop')
        else:
            raise AssertionError('unexpected provider call '+str(number))
        self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
    except Exception:
        errors.append(traceback.format_exc())
        raise
f.FixtureProvider.do_POST=response

listener=socket.socket(socket.AF_UNIX); listener.bind(str(proxy_path)); listener.listen(); listener.settimeout(.2)

def proxy(client):
    upstream=socket.socket(socket.AF_UNIX); upstream.connect(str(f.sockpath))
    connections.extend([client,upstream]); suppress=threading.Event(); target=[None]
    def incoming():
        try:
            with client.makefile('rb') as source:
                for line in source:
                    value=json.loads(line)
                    if value.get('type')=='primary_client_input':
                        request=value['request']; submissions.append(request)
                        with lock:
                            if proxy_state['drop']==request['input']['delivery']:
                                proxy_state['drop']=None; target[0]=request['input']['id']; suppress.set()
                    upstream.sendall(line)
        except (OSError,ValueError): pass
        finally:
            try: upstream.shutdown(socket.SHUT_RDWR)
            except OSError: pass
    threading.Thread(target=incoming,daemon=True).start()
    try:
        with upstream.makefile('rb') as source:
            for line in source:
                value=json.loads(line)
                if suppress.is_set():
                    if value.get('type')=='primary_input_receipt' and value.get('receipt',{}).get('id')==target[0]:
                        dropped.set(); break
                    continue
                client.sendall(line)
    except (OSError,ValueError): pass
    finally:
        for stream in (client,upstream):
            try: stream.shutdown(socket.SHUT_RDWR)
            except OSError: pass
            stream.close()

def accept():
    while not closed.is_set():
        try: client,_=listener.accept()
        except socket.timeout: continue
        except OSError: return
        if proxy_state['blocked']: client.close(); continue
        threading.Thread(target=proxy,args=(client,),daemon=True).start()
threading.Thread(target=accept,daemon=True).start()

def rpc(kind,**fields):
    f.counter+=1; ident=f.counter
    f.send({'type':kind,'id':ident,**fields})
    return f.until(lambda event:event.get('id')==ident and event.get('type')!='ack')
def workspace(action,**fields):
    event=rpc('workspace',request={'action':action,**fields}); assert event['type']=='workspace_response',event
    return event['response']['value']
def command(tid,text):
    row=next(row for row in json.loads((f.home/'testers.json').read_text()) if row['id']==tid)
    target=Path(row['debug_cmd_path']); reply=Path(row['debug_response_path'])
    if reply.exists(): reply.unlink()
    temporary=target.with_suffix('.next'); temporary.write_text(text); os.replace(temporary,target)
    wait(lambda:reply.exists() and reply.stat().st_size>0,text)
    value=reply.read_text(); reply.unlink()
    with (f.ROOT/'tui-input-commands.jsonl').open('a') as out: out.write(json.dumps({'tester':tid,'command':text,'response':value})+'\n')
    return value

def state(tid, test):
    def check():
        try: return test(json.loads(command(tid,'state')))
        except ValueError:return False
    wait(check,'TUI state')
def frame(tid,name):
    raw=command(tid,'screen-json'); (f.ROOT/(name+'.json')).write_text(raw); frames.append(name); return raw

def spawn(session,cols):
    wrapper=f.ROOT/('client-'+str(cols)+'.sh')
    args=[f.BIN,'--no-update','--no-selfdev','--provider-profile','wp09-fixture','--model','fixture','--socket',str(proxy_path),'--resume',session]
    wrapper.write_text('#!/bin/sh\nexec '+' '.join(map(shlex.quote,args)))
    wrapper.chmod(0o700)
    f.debug('tester:spawn '+json.dumps({'cwd':str(f.project),'binary':str(wrapper),'cols':cols,'rows':32 if cols==120 else 24}))
    tid=json.loads((f.home/'testers.json').read_text())[-1]['id'];testers.append(tid)
    wait(lambda:session in command(tid,'startup-context-state'),'attached tester')
    command(tid,'keys:esc,esc'); frame(tid,'attached-'+str(cols)); return tid
result={'status':'failed','binary':f.BIN,'root':str(f.ROOT)}
try:
    f.start(); f.client.settimeout(120)
    workspace('initialize',request=str(uuid.uuid4()))
    launch={'request':str(uuid.uuid4()),'expected_revision':workspace('status')['revision'],'input':{'placement':{'kind':'standalone','root':str(f.project)},'cwd':{'kind':'existing','path':str(f.project)},'agent':None,'model':None,'selfdev':False}}
    created=rpc('primary_launch',request=launch); assert created['response']['status']=='launched',created
    session=created['response']['record']['session']
    tid=spawn(session,120)
    command(tid,'set_input:TUI-FIRST'); command(tid,'keys:enter')
    assert first_entered.wait(60) and dropped.wait(60)
    wait(lambda:len(submissions)>=2,'exact first replay')
    assert submissions[0]==submissions[1],submissions
    assert len(captures)==1
    first_release.set(); state(tid,lambda value:value.get('processing') is False)
    wait(lambda:(f.project/'first-effect').exists(),'first effect')
    assert (f.project/'first-effect').read_text()=='x' and len(captures)==2
    assert 'DURABLE TUI RESULT 2' in frame(tid,'recovered-wide')
    dropped.clear(); proxy_state['drop']='safe_boundary'
    command(tid,'set_input:TUI-HOLD'); command(tid,'keys:enter')
    assert soft_entered.wait(60)
    image=f.project/'fixture.png'
    image.write_bytes(base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aV3sAAAAASUVORK5CYII='))
    proxy_state['blocked']=True
    command(tid,'set_input:TUI-SOFT '+str(image)); command(tid,'keys:enter')
    assert dropped.wait(60),'soft input acceptance was not exercised'
    soft=next(request for request in submissions if request['input']['delivery']=='safe_boundary')
    assert soft['input']['images'],soft
    f.debug('tester:'+tid+':stop'); testers.remove(tid)
    proxy_state['blocked']=False
    tid=spawn(session,80)
    wait(lambda:sum(request['input']['id']==soft['input']['id'] for request in submissions)>=2,'soft input replay after client replacement')
    copies=[request for request in submissions if request['input']['id']==soft['input']['id']]
    assert all(request==soft for request in copies)
    assert len(captures)==3
    soft_release.set(); state(tid,lambda value:value.get('processing') is False)
    wait(lambda:(f.project/'soft-effect').exists(),'soft effect')
    assert (f.project/'soft-effect').read_text()=='x' and len(captures)==5
    assert 'image_url' in json.dumps(captures[3])
    assert 'DURABLE TUI RESULT 5' in frame(tid,'recovered-narrow')
    stored=json.loads((f.home/'sessions'/f'{session}.json').read_text())
    assert len(stored['primary_inputs'])==3
    assert len({receipt['id'] for receipt in stored['primary_inputs']})==3
    assert not errors,errors
    result.update(status='passed',session=session,exact_transport_replay=True,client_replacement=True,complete_soft_image=True,one_effect_each=True,provider_calls=len(captures),frames=frames)
except Exception:
    result['error']=traceback.format_exc()
finally:
    first_release.set(); soft_release.set()
    cleanup={}
    for tid in list(testers):
        try:cleanup[tid]=f.debug('tester:'+tid+':stop')
        except Exception as error:cleanup[tid]=str(error)
    closed.set(); listener.close()
    for connection in connections:
        try:connection.shutdown(socket.SHUT_RDWR)
        except OSError:pass
        connection.close()
    if f.reader:f.reader.close()
    if f.client:f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:f.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:f.proc.kill();f.proc.wait(timeout=10)
    cleanup['daemon_exit']=f.proc.returncode if f.proc else None
    f.http.shutdown();f.http.server_close();f.log.close()
    (f.ROOT/'tui-input-result.json').write_text(json.dumps(result,indent=2))
    (f.ROOT/'tui-input-cleanup.json').write_text(json.dumps(cleanup,indent=2))
    (f.ROOT/'tui-input-submissions.json').write_text(json.dumps(submissions,indent=2))
    (f.ROOT/'tui-input-provider.json').write_text(json.dumps(captures,indent=2))
    print(json.dumps(result,indent=2))
if result['status']!='passed':raise SystemExit(1)
