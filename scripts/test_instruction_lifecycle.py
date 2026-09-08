#!/usr/bin/env python3
"""Combined source-edit/profile/skill/context/recovery journey on a real daemon.

All instructions used for assertions are synthetic. Provider requests terminate
at the local recording fixture. No real project/store or paid model is used.
"""
import json
import subprocess

import test_instruction_manager_mutations as manager

fixture = manager.f


def request(kind, terminal_type='done', **fields):
    fixture.counter += 1
    ident = fixture.counter
    begin = len(fixture.events)
    fixture.send({'type': kind, 'id': ident, **fields})
    terminal = fixture.until(lambda event: event.get('id') == ident and
                             event.get('type') in (terminal_type, 'error', 'context_request_rejected'))
    events = fixture.events[begin:]
    assert terminal['type'] == terminal_type, events
    return events


def event_of(events, kind):
    return next(event for event in events if event.get('type') == kind)


def status():
    value = dict(event_of(request('get_agent_status', include_instructions=True), 'agent_status'))
    value.pop('id')
    return value


def replay_instructions(timeline):
    return next(event for event in timeline if event['event'] == 'session_instructions')


def replay_history(timeline):
    return [event for event in timeline if event['event'] != 'session_instructions']


def replay_profiles(timeline):
    return [event['content'] for event in timeline
            if event['event'] == 'display_message' and event['role'] == 'agent_profile']


def source_agent(store, name, body):
    fixture.source(store, 'agents', name, 'agent', body,
                   f'name: {name}\ndescription: Synthetic integration agent\navailability: both\n')


def progress(message):
    print('JCODE_PROGRESS ' + json.dumps({'message': message}), flush=True)


try:
    fixture.start()
    session = fixture.subscribe()
    store = fixture.home / 'instructions'
    source_agent(store, 'integration-a', 'SYNTHETIC PRIMARY A1')
    source_agent(store, 'integration-b', 'SYNTHETIC PRIMARY B1')
    fixture.git(store, 'add', 'agents/integration-a.md', 'agents/integration-b.md')
    fixture.git(store, 'commit', '-m', 'synthetic integration agents')
    skill = fixture.project / '.jcode/skills/integration/SKILL.md'
    skill.parent.mkdir(parents=True)
    skill.write_text('---\nname: integration-skill\ndescription: Synthetic skill\n---\nSYNTHETIC SKILL OLD')

    request('set_agent', agent='global:integration-a')
    initial = status()
    assert not initial['first_provider_dispatched']
    assert not initial.get('active_transition_message_id')
    assert 'SYNTHETIC PRIMARY A1' in initial['system_prompt']
    request('activate_skill', skill='integration-skill')
    activated = status()
    assert 'SYNTHETIC SKILL OLD' in activated['active_skill']
    assert not fixture.posts
    request('message', content='SYNTHETIC FIRST TURN')
    assert len(fixture.posts) == 1
    original_payload = json.loads(fixture.posts[-1])
    original_system = [item for item in original_payload['messages'] if item['role'] == 'system']
    assert original_system

    draft = manager.begin('integration-a', {'action': 'edit'})
    changed = manager.update(draft, 'SYNTHETIC PRIMARY A2')
    manager.save(changed)
    skill.write_text(skill.read_text().replace('SKILL OLD', 'SKILL NEW'))
    after_edit = status()
    assert after_edit['system_prompt'] == activated['system_prompt']
    assert after_edit['active_skill'] == activated['active_skill']
    request('message', content='SYNTHETIC SECOND TURN')
    assert len(fixture.posts) == 2
    second_payload = json.loads(fixture.posts[-1])
    assert [item for item in second_payload['messages'] if item['role'] == 'system'] == original_system
    assert 'SYNTHETIC SKILL OLD' in fixture.posts[-1]
    assert 'SYNTHETIC SKILL NEW' not in fixture.posts[-1]
    progress('Managed source Save leaves exact active system and skill unchanged across provider requests')

    before_switch = manager.complete_replay(session)
    before_context = event_of(request('get_context_editor_snapshot', terminal_type='context_editor_snapshot'), 'context_editor_snapshot')['snapshot']
    request('set_agent', agent='global:integration-b')
    switched = status()
    active_id = switched['active_transition_message_id']
    assert switched['system_prompt'] == activated['system_prompt']
    assert switched['active_skill'] == activated['active_skill']
    assert len(fixture.posts) == 2
    after_switch = manager.complete_replay(session)
    before_history = replay_history(before_switch)
    after_history = replay_history(after_switch)
    assert after_history[:len(before_history)] == before_history
    profile = replay_profiles(after_switch)[-1]
    assert 'SYNTHETIC PRIMARY B1' in profile
    request('set_agent', agent='global:integration-b')
    assert manager.complete_replay(session) == after_switch

    snapshot = event_of(request('get_context_editor_snapshot', terminal_type='context_editor_snapshot'), 'context_editor_snapshot')['snapshot']
    for field in ('model', 'route', 'provider_name'):
        assert snapshot[field] == before_context[field], field
    locked = next(message for message in snapshot['messages'] if message['message_id'] == active_id)
    assert locked['active_agent_profile']
    fixture.counter += 1
    ident = fixture.counter
    fixture.send({'type': 'preview_context_ranges', 'id': ident,
                  'expected_context_revision': snapshot['context_revision'],
                  'expected_transcript_digest': snapshot['transcript_digest'],
                  'ranges': [{'start_message_id': active_id, 'end_message_id': active_id}]})
    rejected = fixture.until(lambda event: event.get('id') == ident and event.get('type') in ('context_range_closure_preview', 'context_request_rejected', 'error'))
    assert rejected['type'] == 'context_request_rejected', rejected
    assert len(fixture.posts) == 2
    request('rewind', terminal_type='history', message_index=1)
    rewound = manager.complete_replay(session)
    assert replay_profiles(rewound) == [profile]
    assert status()['active_transition_message_id'] == active_id
    request('rewind_undo', terminal_type='history')
    assert manager.complete_replay(session) == after_switch

    split = event_of(request('split', terminal_type='split_response'), 'split_response')['new_session_id']
    clone = manager.complete_replay(split)
    assert replay_instructions(clone) == replay_instructions(after_switch)
    assert replay_profiles(clone) == replay_profiles(after_switch)
    progress('Append-only selection, same-agent no-op, context lock, rewind/undo and split preserve authority')

    request('activate_skill', skill='integration-skill')
    assert 'SYNTHETIC SKILL NEW' in status()['active_skill']
    request('set_agent', agent='global:integration-a', replace=True)
    replaced = status()
    assert 'SYNTHETIC PRIMARY A2' in replaced['system_prompt']
    assert not replaced.get('active_transition_message_id')
    assert 'SYNTHETIC SKILL NEW' in replaced['active_skill']
    assert len(fixture.posts) == 2
    request('message', content='SYNTHETIC REPLACED TURN')
    assert len(fixture.posts) == 3
    assert 'SYNTHETIC PRIMARY A2' in fixture.posts[-1]

    # Process reconstruction must not require the current source files.
    stable = status()
    source_agent(store, 'integration-a', 'SYNTHETIC PRIMARY A3')
    fixture.client.close()
    fixture.reader.close()
    fixture.proc.terminate()
    fixture.proc.wait(timeout=15)
    fixture.start()
    assert fixture.subscribe(session) == session
    assert status() == stable
    assert 'SYNTHETIC PRIMARY A2' in replay_instructions(manager.complete_replay(session))['system_prompt']
    request('clear')
    cleared = status()
    assert cleared['agent_id'] == 'integration-a'
    assert 'SYNTHETIC PRIMARY A3' in cleared['system_prompt']
    assert not cleared.get('active_skill')
    assert not cleared['first_provider_dispatched']
    assert len(fixture.posts) == 3

    # Listing is a recovery route even when the unselected fallback is damaged.
    (store / 'agents/jcode.md').write_text('---\nid: jcode\nkind: agent\n---\nINVALID')
    catalog = request('get_agent_catalog')
    assert 'integration-a' in json.dumps(catalog)
    assert 'integration-b' in json.dumps(catalog)
    progress('Explicit replacement, reinvocation, daemon restart, clear and damaged-fallback catalog recovery passed')
    result = {'binary': subprocess.check_output([fixture.BIN, '--version'], text=True).strip(),
              'artifact': str(fixture.ROOT), 'localhost_model_requests': len(fixture.posts),
              'source_edit_freezing': True, 'append_only_switch': True, 'context_lock': True,
              'rewind_undo': True, 'split': True, 'reinvocation': True, 'replacement': True,
              'daemon_restart': True, 'clear_current_source': True, 'catalog_recovery': True}
    (fixture.ROOT / 'lifecycle-result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)
finally:
    (fixture.ROOT / 'events.json').write_text(json.dumps(fixture.events))
    (fixture.ROOT / 'provider-posts.json').write_text(json.dumps(fixture.posts))
    if fixture.client:
        fixture.client.close()
    if fixture.reader:
        fixture.reader.close()
    if fixture.proc and fixture.proc.poll() is None:
        fixture.proc.terminate()
        try:
            fixture.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            fixture.proc.kill()
            fixture.proc.wait()
    fixture.http.shutdown()
    fixture.log.close()
