#!/usr/bin/env python3
"""Private PTY + shared-host task-monitor acceptance. Uses localhost inference only."""
import argparse, hashlib, shutil, uuid, fcntl, http.server, json, os, pty, select, signal, socket, sqlite3, struct, subprocess, tempfile, termios, threading, time
from pathlib import Path

parser=argparse.ArgumentParser()
parser.add_argument('--binary',required=True)
parser.add_argument('--evidence-parent',required=True)
parser.add_argument('--fixture-test-binary')
args=parser.parse_args()
assert os.environ.get('JCODE_TEST_STATE_ROOT'), 'Use scripts/run_isolated_test.py'
BINARY=str(Path(args.binary).resolve())
ROOT=Path(tempfile.mkdtemp(prefix='monitor-',dir=args.evidence_parent));ROOT.chmod(0o700)
(ROOT/'fixture-script.py').write_text(Path(__file__).read_text())
with open(BINARY,'rb') as binary_file:
    (ROOT/'binary.json').write_text(json.dumps({'path':BINARY,'sha256':hashlib.file_digest(binary_file,'sha256').hexdigest()},indent=2))
HOME=ROOT/'state';HOME.mkdir();PROJECT=ROOT/'project';PROJECT.mkdir();HUMAN=ROOT/'home';HUMAN.mkdir()
SOCKET=Path(os.environ['JCODE_RUNTIME_DIR'])/'wp05.sock'
archive_directory=None
archive_identity=None
posts=[];lock=threading.Lock();workers=[];connections=[];results={};server=None
class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        body=json.dumps({'data':[{'id':'fixture','context_length':200000}]}).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with lock:
            posts.append(body);n=len(posts)
            with (ROOT/'requests.jsonl').open('a') as f:f.write(json.dumps(body)+'\n')
        messages=body['messages'];last=json.dumps(messages[-1]);text='FIXTURE DONE';tool=None
        if 'BOOTSTRAP' in last:text='BOOTSTRAP OK'
        elif any(m.get('role')=='system' and 'SYNTHETIC CHILD SYSTEM' in str(m.get('content')) for m in messages):text='CHILD REPLY'
        elif messages[-1].get('role')=='tool':text='PARENT DONE'
        elif 'PARENT_CHILD' in last:
            tool=('subagent',{'agent':'fixture','model_alias':'fixture','permission':'read_only','prompt':'CHILD ONLY','disable_startup_context':True})
        elif 'PARENT_BASH' in last:
            tool=('bash',{'command':"printf completed-effect > effect; printf LIVE_PREFIX; i=0; while [ $i -lt 600 ]; do printf ' live-%s\\n' \"$i\"; i=$((i+1)); sleep 0.05; done; printf UNSTOPPED > escaped"})
        elif 'summary' in json.dumps(body.get('response_format',{})).lower() or 'file_change_digest' in json.dumps(messages):
            text=json.dumps({'summary':'Synthetic native summary','file_change_digest':'No files changed.','warnings':[]})
        delta={'role':'assistant','content':text};finish='stop'
        if tool:
            name,fields=tool;fields['intent']='native monitor fixture';delta={'role':'assistant','tool_calls':[{'index':0,'id':f'fixture-{n}','type':'function','function':{'name':name,'arguments':json.dumps(fields)}}]};finish='tool_calls'
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Connection','close');self.end_headers()
        try:
            for d,f in [(delta,None),({},finish)]:
                event={'id':'fixture','object':'chat.completion.chunk','model':'fixture','choices':[{'index':0,'delta':d,'finish_reason':f}]}
                self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
        except BrokenPipeError:pass
http=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);http.daemon_threads=True
threading.Thread(target=http.serve_forever,daemon=True).start()
(HOME/'config.toml').write_text(f'''[features]
memory=false
swarm=false
[ambient]
enabled=false
[sponsors]
enabled=false
[display]
debug_socket=true
[providers.wp05-fixture]
type="openai-compatible"
base_url="http://127.0.0.1:{http.server_port}/v1"
auth="none"
requires_api_key=false
default_model="fixture"
models=[{{id="fixture",context_window=200000}}]
''')
ENV=dict(os.environ,HOME=str(HUMAN),JCODE_HOME=str(HOME),JCODE_RUNTIME_DIR=str(SOCKET.parent),JCODE_SOCKET=str(SOCKET),JCODE_DEBUG_CONTROL='1',JCODE_NO_TELEMETRY='1',TERM='xterm-256color')
for key in ['JCODE_SESSION_ID','JCODE_CLIENT_SELFDEV','JCODE_RUNTIME_PROVIDER','JCODE_ACTIVE_PROVIDER']:ENV.pop(key,None)
BASE=[BINARY,'--no-update','--no-selfdev','--provider-profile','wp05-fixture','--model','fixture','-C',str(PROJECT)]
log=(ROOT/'server.log').open('ab')
def run(command):
    result=subprocess.run(command,env=ENV,text=True,capture_output=True,timeout=90)
    assert result.returncode==0,result.stderr[-4000:]
    return result.stdout

def wait(predicate,why,seconds=25):
    deadline=time.monotonic()+seconds
    while True:
        value=predicate()
        if value:return value
        assert time.monotonic()<deadline,why
        time.sleep(.05)

def connect():
    c=socket.socket(socket.AF_UNIX);c.settimeout(30);c.connect(str(SOCKET));r=c.makefile('rb');connections.append((c,r));return c,r

def send(c,data):c.sendall((json.dumps(data)+'\n').encode())
def until(r,predicate):
    while True:
        line=r.readline();assert line,'server disconnected';event=json.loads(line)
        with (ROOT/'events.jsonl').open('a') as f:f.write(json.dumps(event)+'\n')
        if predicate(event):return event

def request(c,r,data,kind):
    send(c,data);event=until(r,lambda e:e.get('id')==data['id'] and e.get('type') in (kind,'error'))
    assert event['type']==kind,event;return event

def stop_fixture_work():
    index=HOME/'execution/index.sqlite'
    if not index.exists():return []
    db=sqlite3.connect(index)
    rows=db.execute("SELECT r.id,t.endpoint,t.auth_key,t.id,t.protocol_version FROM runs r JOIN runtimes t ON t.id=r.owner WHERE r.state IN ('prepared','queued','running')").fetchall()
    outcomes=[]
    for run_id,endpoint,key,owner,version in rows:
        outcome={'run_id':run_id}
        try:
            control=socket.socket(socket.AF_UNIX);control.settimeout(5);control.connect(endpoint)
            control.sendall((json.dumps({'version':version,'instance':owner,'key':key,'run_id':run_id,'action':{'operation':'stop','cause':'human_cancellation'}})+'\n').encode())
            stream=control.makefile('rb');outcome['reply']=json.loads(stream.readline());stream.close();control.close()
        except Exception as error:outcome['error']=str(error)
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            state=db.execute('SELECT state FROM runs WHERE id=?',(run_id,)).fetchone()[0]
            if state not in ('running','queued','prepared'):break
            time.sleep(.05)
        outcome['state']=state;outcomes.append(outcome)
    db.close();(ROOT/'owned-work-cleanup.json').write_text(json.dumps(outcomes,indent=2))
    return [row for row in outcomes if row['state'] in ('running','queued','prepared')]

class Tui:
    def __init__(self,parent):
        self.master,slave=pty.openpty();self.cmd=ROOT/'tui.cmd';self.response=ROOT/'tui.response';self.raw=ROOT/'tui.raw';self.error=(ROOT/'tui.stderr').open('ab')
        self.cmd.touch(mode=0o600);self.response.touch(mode=0o600);self.size=[140,32]
        fcntl.ioctl(self.master,termios.TIOCSWINSZ,struct.pack('HHHH',32,140,0,0))
        env=dict(ENV,JCODE_CLIENT_SELFDEV='1',JCODE_DEBUG_CMD_PATH=str(self.cmd),JCODE_DEBUG_RESPONSE_PATH=str(self.response))
        self.proc=subprocess.Popen(BASE+['--socket',str(SOCKET),'--resume',parent,'--debug-socket'],env=env,stdin=slave,stdout=slave,stderr=self.error,start_new_session=True);os.close(slave);workers.append(self)
        def drain():
            with self.raw.open('ab') as f:
                while self.proc.poll() is None:
                    try:
                        if select.select([self.master],[],[],.1)[0]:f.write(os.read(self.master,65536));f.flush()
                    except OSError:break
        threading.Thread(target=drain,daemon=True).start()
        wait(lambda:self.cmd.exists(),'TUI debug endpoint did not start',45)
        self.debug('enable');self.keys(b'\x1b\x1b')
    def keys(self,data):os.write(self.master,data)
    def resize(self,w,h):
        self.size=[w,h];fcntl.ioctl(self.master,termios.TIOCSWINSZ,struct.pack('HHHH',h,w,0,0));os.kill(self.proc.pid,signal.SIGWINCH)
        def resized():
            try:return json.loads(self.debug('screen-json')).get('terminal_size')==self.size
            except json.JSONDecodeError:return False
        wait(resized,'TUI did not render resized geometry')
    def click(self,x,y):self.keys(f'\x1b[<0;{x+1};{y+1}M\x1b[<0;{x+1};{y+1}m'.encode())
    def debug(self,command):
        assert self.proc.poll() is None,(ROOT/'tui.stderr').read_text(errors='replace')[-4000:]
        self.response.unlink(missing_ok=True);temp=self.cmd.with_suffix('.next');temp.write_text(command);os.replace(temp,self.cmd)
        wait(lambda:self.response.exists() and self.response.stat().st_size>0,'No TUI response for '+command,15)
        text=self.response.read_text();self.response.unlink();return text
    def state(self):
        state=json.loads(self.debug('task-monitor-state'));(ROOT/'last-monitor-state.json').write_text(json.dumps(state,indent=2));return state
    def frame(self,name,contains):
        def capture():
            text=self.debug('screen-json')
            (ROOT/(name+'.last.txt')).write_text(text)
            try:frame=json.loads(text)
            except json.JSONDecodeError:return None
            return text if frame.get('terminal_size')==self.size and contains in frame.get('rendered_text',{}).get('overlay_text','') else None
        text=wait(capture,'Expected frame '+contains);(ROOT/(name+'.json')).write_text(text);return text
    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait(timeout=10)
        os.close(self.master);self.error.close()
try:
    run(BASE+['run','--json','BOOTSTRAP'])
    (HOME/'instructions/agents/fixture.md').write_text('---\nid: fixture\nkind: agent\nname: Fixture\ndescription: Synthetic fixture\navailability: both\n---\nSYNTHETIC PROFILE')
    (HOME/'instructions/system/subagent.md').write_text('---\nid: subagent\nkind: system\n---\nSYNTHETIC CHILD SYSTEM')
    (HOME/'instructions/model-roster.toml').write_text('[aliases.fixture]\ndescription="Synthetic alias"\nmodels=["wp05-fixture:fixture"]\n')
    server=subprocess.Popen(BASE+['--socket',str(SOCKET),'--debug-socket','serve','--server-name','wp05-native'],env=ENV,stdout=log,stderr=log)
    wait(lambda:SOCKET.exists(),'server did not start',45);c,r=connect()
    send(c,{'type':'subscribe','id':1,'working_dir':str(PROJECT),'selfdev':False,'agent':'fixture'})
    events=[]
    while True:
        event=json.loads(r.readline());events.append(event)
        if event.get('type')=='done' and event.get('id')==1:break
        assert event.get('type')!='error',event
    parent=next(e['session_id'] for e in events if e.get('type')=='session')
    tui=Tui(parent);tui.keys(b'/tasks\r');wait(lambda:tui.state().get('visible'),'tasks did not open')
    for w,h in [(140,32),(80,24),(60,24),(48,12),(47,11)]:
        tui.resize(w,h);tui.keys(b'r');tui.frame(f'monitor-{w}x{h}','48' if w<48 else 'Tasks')
    tui.resize(80,24);tui.keys(b'?');tui.frame('actions-80x24','Storage review');tui.click(3,5)
    wait(lambda:tui.state().get('all_sessions'),'mouse scope action did not activate');tui.keys(b'a');wait(lambda:not tui.state().get('all_sessions'),'keyboard scope action did not return')
    # Input is delivered through actual PTY bytes, not a reducer-only invocation.
    tui.keys(b'\x1b[200~NOT-COMPOSER\x1b[201~');assert tui.debug('input').strip()=='input: ""'
    send(c,{'type':'message','id':10,'content':'PARENT_BASH','no_reply':False})
    wait(lambda:(PROJECT/'effect').exists(),'native command did not start')
    wait(lambda:any(row['tool']=='bash' for row in tui.state().get('items',[])),'active bash missing')
    tui.keys(b'\r');tui.frame('live-output','live-');tui.keys(b'\x1b[H');tui.frame('retained-output-prefix','LIVE_PREFIX');tui.keys(b'\x1b[F');wait(lambda:tui.state()['follow'],'End did not resume following');tui.keys(b'f');wait(lambda:not tui.state()['follow'],'pause did not activate');paused=tui.state();time.sleep(.2);assert tui.state()['output_end']==paused['output_end']
    tui.keys(b'f');wait(lambda:tui.state()['output_end']>paused['output_end'],'follow did not resume')
    selected=tui.state()['selected'];tui.keys(b's')
    def terminal():
        db=sqlite3.connect(f'file:{HOME}/execution/index.sqlite?mode=ro',uri=True)
        row=db.execute('SELECT state FROM runs WHERE id=?',(selected,)).fetchone();db.close();return row and row[0] in ('cancelled','interrupted')
    wait(terminal,'actual owned bash did not stop');assert (PROJECT/'effect').read_text()=='completed-effect';assert not (PROJECT/'escaped').exists()
    wait(lambda:tui.state()['selected']==selected and tui.state()['selected_state']=='cancelled','selected completion moved to another row')
    tui.frame('cancelled-selected','Cancelled');until(r,lambda e:e.get('id')==10 and e.get('type') in ('done','error'))
    results['native_stop_partial_and_pinned_selection']=True
    calls_before_input=len(posts)
    tui.click(4,4);wait(lambda:tui.state()['content']=='input','visible Input tab did not activate')
    for w,h in [(140,32),(80,24),(60,24),(48,12)]:
        tui.resize(w,h);frame=tui.frame(f'tool-input-{w}x{h}','printf completed-effect')
        assert 'i Input' in frame and 'o Output' in frame and 'm Info' in frame,'input/output/info choices were not visible'
    tui.resize(80,24);tui.keys(b'v');tui.frame('tool-input-receipt','session_id');tui.keys(b'v')
    tui.keys(b'm');tui.frame('tool-info',selected);tui.keys(b'o');wait(lambda:tui.state()['content']=='output' and not tui.state()['info'],'Output key did not activate')
    tui.frame('tool-output-visible-tabs','Cancelled')
    assert len(posts)==calls_before_input,'Input inspection reran an effect or invoked inference'
    results['native_visible_input_arguments_and_tabs']=True
    tui.keys(b'\x1b');time.sleep(.1);tui.keys(b'1')
    send(c,{'type':'message','id':12,'content':'PARENT_BASH','no_reply':False})
    wait(lambda:any(row['tool']=='bash' and row['state']=='running' and row['id']!=selected for row in tui.state().get('items',[])),'second native command missing')
    second=tui.state()['selected'];assert second!=selected
    tui.keys(b'b')
    def backgrounded():
        db=sqlite3.connect(f'file:{HOME}/execution/index.sqlite?mode=ro',uri=True);row=db.execute('SELECT background FROM runs WHERE id=?',(second,)).fetchone();db.close();return row and row[0]==1
    wait(backgrounded,'background promotion did not retain original run identity')
    until(r,lambda e:e.get('id')==12 and e.get('type') in ('done','error'))
    record=request(c,r,{'type':'task_monitor','id':13,'request':{'action':'inspect','run_id':second}},'task_monitor_response')['response']['row']
    assert record['force_stop_available'],'native command did not advertise owned force control'
    tui.keys(b'S')
    def force_terminal():
        db=sqlite3.connect(f'file:{HOME}/execution/index.sqlite?mode=ro',uri=True);row=db.execute('SELECT state FROM runs WHERE id=?',(second,)).fetchone();db.close();return row and row[0]=='cancelled'
    wait(force_terminal,'force stop did not stop actual owned process');assert not (PROJECT/'escaped').exists()
    assert server.poll() is None,'force stop killed shared server'
    results['native_background_and_force_stop']=True
    tui.keys(b'2');request(c,r,{'type':'message','id':11,'content':'PARENT_CHILD','no_reply':False},'done')
    wait(lambda:any(row.get('child_id') for row in tui.state().get('items',[])),'completed child missing')
    for _ in range(110):
        state=tui.state()
        if state.get('selected_child'):break
        items=state['items'];current=next(i for i,row in enumerate(items) if row['id']==state['selected']);target=next(i for i,row in enumerate(items) if row.get('child_id'))
        tui.keys(b'\x1b[A' if target<current else b'\x1b[B');time.sleep(.05)
    else:raise AssertionError('child row not selectable')
    child=state['selected_child'];selected_child_run=state['selected'];count=len(posts)
    tui.keys(b'\x1b[C');tui.frame('child-history-expanded','Child history');tui.keys(b'\x1b')
    wait(lambda:tui.state()['selected']==selected_child_run,'Back lost the child invocation identity')
    # The same dedicated, unattached route used by local TUI child controls.
    session_count=len(list((HOME/'sessions').glob('*.json')))
    direct,dr=connect()
    namespace=request(direct,dr,{'type':'delegation_probe','id':101},'delegation_capabilities')['namespace'];assert Path(namespace)==HOME.resolve()
    request(direct,dr,{'type':'task_monitor_probe','id':102},'task_monitor_capabilities')
    snapshot=request(direct,dr,{'type':'child_context','id':103,'child_id':child,'request':{'type':'get_context_editor_snapshot','id':103,'page_start':0,'page_size':250}},'child_context_response')
    assert snapshot['event']['type']=='context_editor_snapshot' and snapshot['event']['snapshot']['session_id']==child
    dr.close();direct.close()
    forbidden,fr=connect();reply=request(forbidden,fr,{'type':'child_context','id':104,'child_id':child,'request':{'type':'message','id':104,'content':'FORBIDDEN CHILD CHAT','no_reply':False}},'child_context_response');assert reply['event']['type']=='error';fr.close();forbidden.close()
    assert len(list((HOME/'sessions').glob('*.json')))==session_count and len(posts)==count,'Child inspection allocated chat or invoked inference'
    results['native_unattached_child_control_without_chat']=True
    tui.keys(b'c');tui.frame('child-context','Context')
    wait(lambda:json.loads(tui.debug('context-editor-state')).get('loaded_rows',0)>0,'child context did not load')
    assert len(posts)==count,'human opening child context invoked inference'
    tui.resize(60,24);tui.frame('child-context-60x24','Context');tui.resize(80,24)
    def editor():
        state=json.loads(tui.debug('context-editor-state'));(ROOT/'last-child-editor-state.json').write_text(json.dumps(state,indent=2));return state
    revision=editor()['context_revision']
    tui.keys(b'\x1b[6~');wait(lambda:editor()['selected_global_index']==editor()['total_rows']-1,'last child message not selected')
    tui.keys(b's');time.sleep(.05);tui.keys(b'\x1b[A');time.sleep(.05);tui.keys(b's')
    wait(lambda:editor()['phase']=='confirm_range_closure','child range did not receive closure review');tui.keys(b'\r')
    wait(lambda:editor()['staged_ranges']==1,'child range did not stage');tui.keys(b'g')
    wait(lambda:editor()['curator_plan_current'],'exact child curator plan did not load');tui.frame('child-curator-plan','curator')
    tui.keys(b'g');wait(lambda:editor()['phase']=='review_draft','child curator draft did not complete');tui.frame('child-curator-review','review')
    tui.keys(b'a');time.sleep(.05);tui.keys(b'\r');wait(lambda:editor()['context_revision']>revision,'child context did not apply')
    applied_revision=editor()['context_revision'];tui.frame('child-context-applied','Context')
    for _ in range(6):
        if not editor()['curator_workspace_active']:break
        tui.keys(b'\x1b');time.sleep(.1)
    assert not editor()['curator_workspace_active'],'curator workspace did not close'
    tui.keys(b'H');wait(lambda:editor()['history_count']>0,'child transaction history did not load')
    tui.keys(b'r');wait(lambda:editor()['modal']=='revert_confirmation','child undo confirmation did not open');tui.keys(b'\r')
    wait(lambda:editor()['context_revision']>applied_revision,'child undo did not persist');tui.frame('child-context-undo','Context')
    assert len(posts)==count+1,'apply/undo restarted child or duplicated curator inference'
    results['native_child_curator_apply_undo']=True
    count=len(posts)
    for _ in range(5):
        if tui.state().get('child_editor') is None:break
        tui.keys(b'\x1b');time.sleep(.1)
    assert tui.state().get('child_editor') is None and tui.state()['visible'],'did not return to tasks'
    assert len(posts)==count,'closing child context restarted child'
    results['native_child_context_navigation_no_inference']=True
    archive_fixture=None
    if args.fixture_test_binary:
        archive_directory=Path('/Volumes/Active')/('jcode-execution-fixture-wp05-'+uuid.uuid4().hex)
        assert not archive_directory.exists()
        (HOME/'WP05_NATIVE_FIXTURE').write_text('native monitor fixture')
        manifest=ROOT/'archive-setup.json';receipt=ROOT/'archive-fixture.json'
        manifest.write_text(json.dumps({'root':str(HOME),'archive':{'mount':'/Volumes/Active','volume_uuid':'5B3BF7CE-D42A-432A-80C6-A7279A89DC77','directory':archive_directory.name},'result':str(receipt)}))
        seed=subprocess.run([str(Path(args.fixture_test_binary).resolve()),'execution::task_monitor::tests::native_monitor_archive_fixture','--exact','--ignored','--test-threads=1'],env=dict(ENV,JCODE_TASK_MONITOR_FIXTURE=str(manifest)),capture_output=True,text=True,timeout=45)
        (ROOT/'archive-setup.log').write_text(seed.stdout+seed.stderr);assert seed.returncode==0,seed.stdout+seed.stderr
        archive_fixture=json.loads(receipt.read_text());assert Path(archive_fixture['output_path']).exists()
        stat=archive_directory.stat(follow_symlinks=False);archive_identity=(stat.st_dev,stat.st_ino)
    tui.keys(b'g');tui.frame('storage','Storage administration');tui.keys(b'\x1b[200~1\x1b[201~\r');tui.frame('cleanup-review','Cleanup review')
    if archive_fixture:
        frame=tui.frame('cleanup-exact-candidate',archive_fixture['run_id'])
        tui.keys(b'\x1b[6~');tui.frame('cleanup-snapshot-impact',archive_fixture['snapshot_id'])
        assert Path(archive_fixture['output_path']).exists(),'review deleted output without confirmation'
        tui.keys(b'y');wait(lambda:not Path(archive_fixture['output_path']).exists(),'confirmed fixture output was not deleted')
        tui.frame('cleanup-completed','Deleted');results['native_exact_cleanup_confirmation']=True
    tui.keys(b'\x1b');time.sleep(.1)

    results['native_storage_review_no_implicit_deletion']=True
    selected_before=tui.state()['selected'];generation=tui.state()['generation'];calls_before=len(posts)
    server.terminate();server.wait(timeout=15)
    server=subprocess.Popen(BASE+['--socket',str(SOCKET),'--debug-socket','serve','--server-name','wp05-native'],env=ENV,stdout=log,stderr=log)
    wait(lambda:tui.state()['generation']>generation and tui.state()['capability'] is True,'monitor did not reconnect through a fresh capability handshake',45)
    assert tui.state()['selected']==selected_before and tui.state()['visible'],'reconnect lost selected task'
    assert len(posts)==calls_before,'reconnect restarted inference'
    tui.frame('monitor-reconnected','Tasks');results['native_reconnect_without_reexecution']=True
    results['sizes']=[[140,32],[80,24],[60,24],[48,12],[47,11]]
    results['passed']=True
finally:
    try:cleanup_failures=stop_fixture_work()
    except Exception as error:cleanup_failures=[{'cleanup_error':str(error)}]
    if cleanup_failures:results['passed']=False;results['cleanup_failures']=cleanup_failures
    for worker in workers:
        try:worker.close()
        except Exception as error:cleanup_failures.append({'tui_cleanup':str(error)})
    for c,r in connections:
        try:r.close();c.close()
        except Exception:pass
    if server and server.poll() is None:
        server.terminate()
        try:server.wait(timeout=15)
        except subprocess.TimeoutExpired:server.kill();server.wait(timeout=10)
    http.shutdown();http.server_close();log.close()
    if archive_directory and archive_directory.exists():
        assert archive_directory.parent==Path('/Volumes/Active') and archive_directory.name.startswith('jcode-execution-fixture-wp05-')
        stat=archive_directory.stat(follow_symlinks=False)
        if archive_identity==(stat.st_dev,stat.st_ino):
            try:shutil.rmtree(archive_directory)
            except Exception as error:cleanup_failures.append({'archive_cleanup':str(error)})
        else:cleanup_failures.append({'archive_cleanup':'Fixture identity changed or setup did not complete. Preserved '+str(archive_directory)})
    if cleanup_failures:results['passed']=False;results['cleanup_failures']=cleanup_failures
    (ROOT/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps({'evidence':str(ROOT),'results':results}))

    if cleanup_failures:raise RuntimeError('Owned fixture cleanup did not complete; see results.json')
