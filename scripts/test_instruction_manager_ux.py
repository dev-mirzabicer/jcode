#!/usr/bin/env python3
"""Approved type/scope/source UX through real isolated clients and physical keys."""
import fcntl, json, os, signal, struct, subprocess, termios, time
from pathlib import Path
import test_instruction_manager_mutations as m
f=m.f

def type_text(tid,text): m.keys(tid,','.join('space' if ch==' ' else ch for ch in text))
def state(tid,predicate): return f.tester_state(tid,predicate)
def settled(tid): return state(tid,lambda s:s.get('pending_id') is None and s.get('editing',{}).get('pending') is None)
def choose_type(tid,index,label):
    m.keys(tid,'f1,home'+',down'*index+',enter')
    state(tid,lambda s:s.get('category')==label and s.get('pending_id') is None)
def search(tid,text):
    m.keys(tid,'/,ctrl+u');type_text(tid,text);m.keys(tid,'enter')
    state(tid,lambda s:s.get('rows_loaded')==1 and s.get('pending_id') is None)
def select_menu(tid,text):
    type_text(tid,text);m.keys(tid,'enter')
def save_close(tid):
    m.ui(tid,lambda s:s.get('reviewed') and s.get('pending') is None)
    m.keys(tid,'s');m.ui(tid,lambda s:not s.get('reviewed') and not s.get('failed') and s.get('pending') is None)
    m.keys(tid,'esc');m.ui(tid,lambda s:not s.get('visible'));settled(tid)
def stop_last():
    child,master,_,_=m.clients[-1]
    child.terminate();child.wait(timeout=10);os.close(master);m.clients[-1][1]=None

try:
    f.start();session=f.subscribe();store=f.home/'instructions';session_file=f.home/'sessions'/(session+'.json');frozen=json.loads(session_file.read_text())['system_prompt']
    f.source(store,'modules','ux-source','module','GLOBAL ORIGINAL');f.git(store,'add','modules/ux-source.md');f.git(store,'commit','-m','synthetic UX source')
    external=f.project/'.jcode/skills/friendly';external.mkdir(parents=True);(external/'SKILL.md').write_text('---\nname: Design\ndescription: Make synthetic interfaces\n---\nEXTERNAL BODY')
    desired=f.ROOT/'desired-body';desired.write_text('GLOBAL EDIT')
    m.editor=f.ROOT/'local editor.py';m.editor.write_text('import sys,termios\nfrom pathlib import Path\nassert termios.tcgetattr(0)[3]&termios.ICANON\nPath(sys.argv[-1]).write_text(Path('+repr(str(desired))+').read_text())\n')
    tid=m.spawn_client(session,150,40)
    initial=f.frame(tid,'Agents',motion='home');(f.ROOT/'initial.json').write_text(json.dumps(initial,indent=2))
    assert not any('READ ONLY' in row.get('content_preview','') for row in initial['rendered_text']['recent_messages'])
    m.keys(tid,'f4');state(tid,lambda s:s.get('editing',{}).get('visible') and s['editing'].get('pending') is None)
    setup=f.frame(tid,'Project repository controls',motion='home');(f.ROOT/'setup.json').write_text(json.dumps(setup,indent=2));m.keys(tid,'esc');m.ui(tid,lambda s:not s.get('visible'))
    m.operation('project',{'action':'standalone','path':'.jcode/instructions'});m.keys(tid,'r');settled(tid)
    choose_type(tid,3,'Shared modules');search(tid,'ux-source')
    overview=f.frame(tid,'GLOBAL ORIGINAL',motion='home');(f.ROOT/'overview.json').write_text(json.dumps(overview,indent=2))
    m.keys(tid,'e');m.ui(tid,lambda s:s.get('reviewed') and s.get('pending') is None)
    assert (store/'modules/ux-source.md').read_text().endswith('GLOBAL ORIGINAL')
    review=f.frame(tid,'GLOBAL EDIT',motion='end');(f.ROOT/'automatic-review.json').write_text(json.dumps(review,indent=2));save_close(tid)
    desired.write_text('PROJECT EDIT');m.keys(tid,'v');select_menu(tid,'Copy to project and edit');m.ui(tid,lambda s:s.get('reviewed') and s.get('pending') is None);save_close(tid)
    project_store=f.project/'.jcode/instructions';assert (project_store/'modules/ux-source.md').read_text().endswith('PROJECT EDIT');assert (store/'modules/ux-source.md').read_text().endswith('GLOBAL EDIT')
    state(tid,lambda s:s.get('rows_loaded')==1 and s.get('pending_id') is None)
    versions=f.frame(tid,'PROJECT EDIT',motion='home');(f.ROOT/'effective-project.json').write_text(json.dumps(versions,indent=2))
    m.keys(tid,'s');select_menu(tid,'Global');state(tid,lambda s:s.get('browse_scope')=='Global' and s.get('pending_id') is None)
    global_frame=f.frame(tid,'GLOBAL EDIT',motion='home');(f.ROOT/'global-view.json').write_text(json.dumps(global_frame,indent=2))
    m.keys(tid,'v');select_menu(tid,'Copy to project and edit');collision=f.frame(tid,'already exists',motion='home');(f.ROOT/'copy-collision.json').write_text(json.dumps(collision,indent=2))
    m.keys(tid,'esc')
    m.keys(tid,'esc');choose_type(tid,12,'Current session instructions');snapshot=f.frame(tid,'not',motion='home');(f.ROOT/'session-snapshot.json').write_text(json.dumps(snapshot,indent=2))
    choose_type(tid,11,'Repositories & sync');repositories=f.frame(tid,'Global instructions',motion='home');(f.ROOT/'repositories.json').write_text(json.dumps(repositories,indent=2))
    for width,height in [(80,24),(60,24),(24,10)]:
        child,master,_,_=m.clients[-1]
        fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',height,width,0,0));os.kill(child.pid,signal.SIGWINCH)
        choose_type(tid,1,'Skills')
        m.keys(tid,'s');select_menu(tid,'Effective here');state(tid,lambda value:value.get('browse_scope')=='Effective here' and value.get('pending_id') is None)
        view=f.frame(tid,'Design',motion='home');(f.ROOT/f'skills-{width}.json').write_text(json.dumps(view,indent=2))
        text='\n'.join(row.get('content_preview','') for row in view['rendered_text']['recent_messages'])
        assert '[P ext]' in text,text;assert 'SKILL.md' not in text,text;assert 'READ ONLY' not in text
        assert view['terminal_size']==[width,height],view['terminal_size']
    stop_last()
    assert json.loads(session_file.read_text())['system_prompt']==frozen;assert not f.posts
    result={'type_first':True,'scope_versions_and_explicit_edit_ownership':True,'direct_editor_and_automatic_review':True,'global_to_project_copy':True,'external_name_scope_badges_at_80_60_24':True,'setup_and_session_pages':True,'physical_resize_path':True,'current_prompt_unchanged':True,'model_requests':0,'artifact':str(f.ROOT),'binary':subprocess.check_output([f.BIN,'--version'],text=True).strip()}
    (f.ROOT/'ux-result.json').write_text(json.dumps(result,indent=2));print(json.dumps(result),flush=True)
finally:
    for child,master,_,_ in m.clients:
        if child.poll() is None:
            child.terminate()
            try:child.wait(timeout=10)
            except subprocess.TimeoutExpired:child.kill();child.wait()
        if master is not None:
            try:os.close(master)
            except OSError:pass
    (f.ROOT/'events.json').write_text(json.dumps(f.events));(f.ROOT/'provider-posts.json').write_text(json.dumps(f.posts))
    if f.client:f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:f.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:f.proc.kill();f.proc.wait()
    f.http.shutdown();f.log.close()
