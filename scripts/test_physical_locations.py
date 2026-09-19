#!/usr/bin/env python3
"""Activated physical-key/capture compatibility via real daemon editor operations.

Run with scripts/run_isolated_test.py. All source, plans, stores and sessions are
owned fixtures. No model request, real project mutation or volume operation occurs.
"""
import hashlib
import json
import os
import subprocess

import test_instruction_manager as f

if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py to isolate HOME and runtime state')


def git(path, *args):
    subprocess.run(['git', '-C', str(path), *args], env=f.env, check=True,
                   capture_output=True, text=True)


def request(kind, expected, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type': kind, 'id': rid, **fields})
    result = f.until(lambda event: event.get('id') == rid and
                     event.get('type') in (expected, 'startup_context_failed', 'error'))
    assert result['type'] == expected, result
    return result


def create(path):
    request('subscribe', 'done', working_dir=str(path), selfdev=False,
            agent='global:jcode', startup_context_caller='harness_api_create')
    return request('state', 'state')['session_id']


def status():
    return request('get_startup_context_status', 'startup_context_status')['snapshot']


def content(snapshot):
    row, = snapshot['files']
    return request('get_startup_context_file_detail', 'startup_context_file_detail',
                   batch_id=row['batch_id'], spec_id=row['spec_id'],
                   message_id=row['message_id'], expected_sha256=row['sha256'])['detail']['content']


result = {}
try:
    main = f.project
    git(main, 'init', '--quiet')
    git(main, 'config', 'user.name', 'Jcode fixture')
    git(main, 'config', 'user.email', 'fixture@example.invalid')
    (main / 'PLAN.md').write_text('SYNTHETIC MAIN SOURCE\n')
    git(main, 'add', 'PLAN.md')
    git(main, 'commit', '--quiet', '-m', 'fixture')
    linked = f.ROOT / 'linked'
    git(main, 'worktree', 'add', '--quiet', '-b', 'linked', str(linked))
    (linked / 'PLAN.md').write_text('SYNTHETIC LINKED SOURCE\n')
    (linked / 'nested').mkdir()
    alias = f.ROOT / 'alias'
    alias.symlink_to(linked, target_is_directory=True)
    independent = f.ROOT / 'independent'
    git(main, 'clone', '--quiet', '--no-local', str(main), str(independent))
    plain = f.ROOT / 'plain'
    plain.mkdir()
    f.start()
    first = create(main)
    editor = request('open_startup_context_editor', 'startup_context_editor_opened')['editor']
    expected_key = hashlib.sha256(b'git\0' + str((main / '.git').resolve()).encode()).hexdigest()
    assert editor['project']['key_digest'] == expected_key
    assert editor['project']['active_root'] == str(main.resolve())
    args = dict(lease_id=editor['lease']['lease_id'], project_key_digest=expected_key,
                expected_plan_revision=editor['plan_revision'], selection=[{'path': 'PLAN.md'}])
    preview = request('preview_startup_context_selection', 'startup_context_selection_preview', **args)['preview']
    assert preview['selected_count'] == 1 and preview['issue_count'] == 0
    applied = request('apply_startup_context_selection', 'startup_context_apply_status',
                      operation_id='c01-wp01-physical-capture', save_project_default=True, **args)['status']
    assert applied['phase'] == 'succeeded', applied
    request('close_startup_context_editor', 'startup_context_editor_closed',
            lease_id=args['lease_id'], project_key_digest=expected_key)
    initial = status()
    assert content(initial) == 'SYNTHETIC MAIN SOURCE\n'
    created = {first}
    for path in (linked / 'nested', alias):
        session = create(path)
        assert session not in created
        created.add(session)
        current = status()
        assert current['compact']['project']['key_digest'] == expected_key
        assert current['compact']['project']['active_root'] == str(linked.resolve())
        assert content(current) == 'SYNTHETIC LINKED SOURCE\n'
    for path, kind in ((independent, 'git'), (plain, 'directory')):
        create(path)
        current = status()
        assert current['compact']['project']['kind'] == kind
        assert current['compact']['project']['key_digest'] != expected_key
        assert current['compact']['plan_entry_count'] == 0
        assert current['total_files'] == 0
    (main / 'PLAN.md').write_text('CHANGED AFTER CAPTURE\n')
    request('subscribe', 'done', target_session_id=first, working_dir=str(main), selfdev=False)
    resumed = status()
    assert content(resumed) == 'SYNTHETIC MAIN SOURCE\n'
    assert resumed['files'][0]['message_id'] == initial['files'][0]['message_id']
    assert not f.posts, 'identity/editor operations must not infer'
    result = {'binary': subprocess.check_output([f.BIN, '--version'], text=True).strip(),
              'artifact': str(f.ROOT), 'git_key': expected_key,
              'main_saved_plan': True, 'linked_subdirectory_and_alias_capture': True,
              'independent_clone_default_is_distinct': True, 'non_git_default_distinct': True,
              'resume_retains_original_capture': True, 'provider_requests': len(f.posts)}
finally:
    # Owned runtime teardown precedes evidence writes, including failure paths.
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:
            f.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            f.proc.kill()
            f.proc.wait()
    f.http.shutdown()
    f.log.close()
    (f.ROOT / 'events.json').write_text(json.dumps(f.events))
    (f.ROOT / 'provider-posts.json').write_text(json.dumps(f.posts))
    (f.ROOT / 'physical-result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)
