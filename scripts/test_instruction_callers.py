#!/usr/bin/env python3
"""Activated CLI, direct REPL and Harness API instruction selection acceptance.

Private fixture homes, synthetic content and local endpoints only. No inference
request is expected. Reuses the production-manager fixture's daemon ownership.
"""
import json
import socket
import subprocess
import time

import test_instruction_manager as f

bridge = None
api = None
api_reader = None
peer = None
peer_reader = None
api_events = []
api_id = 0
bridge_log = open(f.ROOT / 'bridge.log', 'wb')


def api_request(kind, expected, channel=None, **fields):
    global api_id
    api_id += 1
    connection, reader = channel or (api, api_reader)
    connection.sendall((json.dumps({'v': 1, 'id': api_id, 'req': kind, **fields}) + '\n').encode())
    while True:
        line = reader.readline()
        assert line, 'Harness bridge closed before replying'
        event = json.loads(line)
        api_events.append(event)
        if event.get('reply_to') == api_id:
            assert event['ev'] in expected, event
            return event


def snapshot_ids():
    return {path.stem for path in (f.home / 'sessions').glob('*.json')}


try:
    f.start()
    f.subscribe()
    store = f.home / 'instructions'
    f.source(store, 'agents', 'caller-fixture', 'agent', 'SYNTHETIC CALLER PROFILE',
             'name: caller-fixture\ndescription: Synthetic caller fixture\navailability: both\n')
    cli = [f.BIN, '--no-update', '--no-selfdev', '--provider-profile', 'wp09-fixture',
           '--model', 'fixture', '-C', str(f.project)]
    repl = subprocess.run(cli + ['--agent', 'global:caller-fixture', 'repl'],
                          input='quit\n', env=f.env, capture_output=True, text=True, timeout=30)
    (f.ROOT / 'repl.stdout').write_text(repl.stdout)
    (f.ROOT / 'repl.stderr').write_text(repl.stderr)
    assert repl.returncode == 0, repl.stderr
    before = snapshot_ids()
    invalid = subprocess.run(cli + ['--agent', 'global:missing', 'run', 'SYNTHETIC NO-DISPATCH'],
                             env=f.env, capture_output=True, text=True, timeout=30)
    (f.ROOT / 'run-invalid.stdout').write_text(invalid.stdout)
    (f.ROOT / 'run-invalid.stderr').write_text(invalid.stderr)
    assert invalid.returncode != 0
    assert snapshot_ids() == before, 'invalid CLI creation left an unpublished session'
    assert not f.posts

    api_path = f.ROOT / 'a.sock'
    bridge = subprocess.Popen(cli + ['--socket', str(f.sockpath), 'api-bridge',
                                     '--api-socket', str(api_path)],
                              env=f.env, stdout=bridge_log, stderr=bridge_log)
    deadline = time.monotonic() + 30
    while True:
        assert bridge.poll() is None, (f.ROOT / 'bridge.log').read_text()
        api = socket.socket(socket.AF_UNIX)
        api.settimeout(30)
        try:
            api.connect(str(api_path))
            break
        except (FileNotFoundError, ConnectionRefusedError):
            api.close()
            if time.monotonic() > deadline:
                raise
            time.sleep(0.1)
    api_reader = api.makefile('rb')
    hello = api_request('hello', {'hello_ok'}, min_version=1, max_version=1, client='wp11-synthetic')
    assert {'initial_agent_selection', 'agent_profile_controls', 'workflow_prompt_rendering'} <= set(hello['capabilities'])
    created = api_request('create_session', {'attached'}, working_dir=str(f.project), agent='global:caller-fixture')
    session = created['session']['session_id']
    inspected = api_request('inspect_agent', {'agent_status'}, session_id=session, include_instructions=True)
    assert inspected['agent_id'] == 'caller-fixture'
    assert 'SYNTHETIC CALLER PROFILE' in inspected['system_prompt']
    assert not inspected['first_provider_dispatched']
    peer = socket.socket(socket.AF_UNIX)
    peer.settimeout(30)
    peer.connect(str(api_path))
    peer_reader = peer.makefile('rb')
    peer_channel = (peer, peer_reader)
    api_request('hello', {'hello_ok'}, channel=peer_channel,
                min_version=1, max_version=1, client='wp11-synthetic-peer')
    api_request('attach_session', {'attached'}, channel=peer_channel, session_id=session)
    before = snapshot_ids()
    failed = api_request('create_session', {'error', 'startup_context_creation_failed'},
                         working_dir=str(f.project), agent='global:missing')
    assert snapshot_ids() == before, 'invalid Harness creation left an unpublished session'
    assert not f.posts
    # The rejected create must not destroy the existing session or API connection.
    attached = api_request('attach_session', {'attached'}, session_id=session)
    assert attached['session']['session_id'] == session
    again = api_request('inspect_agent', {'agent_status'}, session_id=session, include_instructions=True)
    assert again['system_prompt'] == inspected['system_prompt']
    second_project = f.ROOT / 'second-project'
    second_project.mkdir()
    (second_project / 'AGENTS.md').write_text('SYNTHETIC SECOND PROJECT')
    f.source(store, 'agents', 'caller-fixture', 'agent', 'SYNTHETIC CALLER UPDATED',
             'name: caller-fixture\ndescription: Synthetic caller fixture\navailability: both\n')
    fresh = api_request('create_session', {'attached'}, working_dir=str(second_project),
                        agent='global:caller-fixture')['session']['session_id']
    assert fresh != session
    fresh_status = api_request('inspect_agent', {'agent_status'}, session_id=fresh,
                               include_instructions=True)
    assert 'SYNTHETIC CALLER UPDATED' in fresh_status['system_prompt']
    assert 'SYNTHETIC SECOND PROJECT' in fresh_status['system_prompt']
    assert not fresh_status['first_provider_dispatched']
    peer_status = api_request('inspect_agent', {'agent_status'}, channel=peer_channel,
                              session_id=session, include_instructions=True)
    assert peer_status['system_prompt'] == inspected['system_prompt']
    default = api_request('create_session', {'attached'}, working_dir=str(f.project))['session']['session_id']
    assert default not in (fresh, session)
    default_status = api_request('inspect_agent', {'agent_status'}, session_id=default,
                                 include_instructions=True)
    assert default_status['agent_id'] == 'jcode', 'new creation must resolve defaults, not retain the last profile'
    assert not f.posts
    result = {'binary': subprocess.check_output([f.BIN, '--version'], text=True).strip(),
              'artifact': str(f.ROOT), 'direct_repl': True, 'invalid_cli_no_orphan': True,
              'harness_capabilities': hello['capabilities'], 'harness_explicit_agent': True,
              'harness_invalid_no_orphan': True, 'harness_recovery_attach': True, 'repeated_create_distinct': True,
              'fresh_project_and_source': True, 'new_create_resolves_defaults': True,
              'peer_keeps_old_session': True,
              'harness_failure_event': failed['ev'], 'provider_requests': 0}
    (f.ROOT / 'caller-result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)
finally:
    (f.ROOT / 'api-events.json').write_text(json.dumps(api_events))
    (f.ROOT / 'events.json').write_text(json.dumps(f.events))
    (f.ROOT / 'provider-posts.json').write_text(json.dumps(f.posts))
    if peer_reader:
        peer_reader.close()
    if peer:
        peer.close()
    if api_reader:
        api_reader.close()
    if api:
        api.close()
    for process in (bridge, f.proc):
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    f.http.shutdown()
    f.log.close()
    bridge_log.close()
