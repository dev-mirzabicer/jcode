#!/usr/bin/env python3
"""WP-06 native checkout service acceptance in an owned isolated daemon namespace.

Run via scripts/run_isolated_test.py with --binary and a short --artifact-dir.
Never touches a real checkout, catalog, credential, provider or mounted volume.
"""
import json
import os
import socket
import subprocess
import time
import uuid
from pathlib import Path

import test_instruction_manager as f

if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')


def uid():
    return str(uuid.uuid4())


def rpc(action, expected=None, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type': 'workspace', 'id': rid, 'request': {'action': action, **fields}})
    event = f.until(lambda e: e.get('id') == rid and e.get('type') in ('workspace_response', 'error'))
    assert event['type'] == 'workspace_response', event
    response = event['response']
    if expected:
        assert response['kind'] == expected, response
    return response['value']


def request(kind, expected, **fields):
    f.counter += 1
    rid = f.counter
    f.send({'type': kind, 'id': rid, **fields})
    event = f.until(lambda e: e.get('id') == rid and e.get('type') in (expected, 'startup_context_failed', 'error'))
    assert event['type'] == expected, event
    return event


def git(root, *args):
    result = subprocess.run(['git', '-C', str(root), *args], env=f.env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (args, result.stdout, result.stderr)
    return result.stdout.strip()


def status():
    return rpc('status', 'status')


def change(action, **fields):
    review = rpc('review', 'review', expected_revision=status()['revision'],
                 change={'action': action, **fields})
    receipt = rpc('apply', 'receipt', request=uid(), review=review['id'])
    assert not receipt['issues'], receipt
    return receipt


def await_clone(rid):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        record = rpc('inspect_clone', 'clone', request=rid)
        if record['state'] in ('ready', 'preparation_failed', 'cancelled', 'recovery_required'):
            assert record['state'] == 'ready', record
            return record
        time.sleep(.15)
    raise AssertionError(('clone did not finish', rid, rpc('inspect_clone', 'clone', request=rid)))


def await_output(clone_id, run_id):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        row = rpc('clone_output', 'clone_output', clone=clone_id,
                  request={'action': 'inspect', 'run_id': run_id})
        assert row['kind'] == 'status', row
        run = row['run']
        if run['state'] in ('completed', 'failed', 'cancelled', 'interrupted'):
            assert run['state'] == 'completed' and run['complete'], run
            assert run['output_bytes'] > 0 and run['tool'] == 'workspace_clone', run
            return run
        time.sleep(.1)
    raise AssertionError(('clone output did not reach terminal publication', run_id))


def clone(spec):
    review = rpc('review_clone', 'clone_review', expected_revision=status()['revision'], spec=spec)
    rid = uid()
    initial = rpc('begin_clone', 'clone', request=rid, review=review['id'])
    assert initial['request'] == rid and initial['review']['id'] == review['id']
    ready = await_clone(rid)
    assert ready['location'] == initial['location']
    assert len(ready['output_runs']) == 1, ready
    await_output(rid, ready['output_runs'][0])
    assert rpc('begin_clone', 'clone', request=rid, review=review['id'])['location'] == ready['location']
    assert rpc('inspect_clone', 'clone', request=rid) == ready
    return ready


result = {}
try:
    source = f.project
    git(source, 'init', '-q', '-b', 'main')
    (source / 'PLAN.md').write_text('SOURCE CONTEXT\n')
    git(source, 'add', 'PLAN.md')
    git(source, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'source')
    commit = git(source, 'rev-parse', 'HEAD')
    (source / 'dirty-untracked').write_text('Do not copy this')
    bare = f.ROOT / 'remote.git'
    subprocess.run(['git', 'clone', '--bare', '-q', str(source), str(bare)],
                   env=f.env, check=True, capture_output=True, timeout=30)
    f.start()
    probe = request('workspace_probe', 'workspace_capabilities')
    assert probe['checkout_version'] == 1 and probe['managed_rollout'] is False
    rpc('initialize', 'status', request=uid())
    volumes = rpc('volumes', 'volumes')
    local = [v for v in volumes if v['writable'] and
             os.stat(v['mount']).st_dev == os.stat(f.ROOT).st_dev]
    assert len({v['uuid'] for v in local}) == 1, local
    volume = local[0]
    project, = change('create_project', name='Checkout fixture') ['targets']
    repo, = change('create_repository', name='Repo', remotes=[]) ['targets']
    change('associate_repository', project=project['id'], repository=repo['id'])

    # An ordinary primary saves the source default through its actual editor.
    request('subscribe', 'done', working_dir=str(source), selfdev=False,
            agent='global:jcode', startup_context_caller='harness_api_create')
    editor = request('open_startup_context_editor', 'startup_context_editor_opened')['editor']
    selection = dict(lease_id=editor['lease']['lease_id'],
                     project_key_digest=editor['project']['key_digest'],
                     expected_plan_revision=editor['plan_revision'],
                     selection=[{'path': 'PLAN.md'}])
    preview = request('preview_startup_context_selection', 'startup_context_selection_preview', **selection)['preview']
    assert preview['issue_count'] == 0, preview
    saved = request('apply_startup_context_selection', 'startup_context_apply_status',
                    operation_id='wp06-source-plan', save_project_default=True, **selection)['status']
    assert saved['phase'] == 'succeeded', saved
    request('close_startup_context_editor', 'startup_context_editor_closed',
            lease_id=selection['lease_id'], project_key_digest=selection['project_key_digest'])

    home = {'kind': 'project', 'id': project['id']}
    destination = f.ROOT / 'independent'
    common = dict(home=home, repository=repo['id'], name='independent',
                  base={'kind': 'branch', 'name': 'main'}, remotes=[],
                  destination={'kind': 'custom', 'volume_uuid': volume['uuid'],
                               'path': str(destination)}, submodules=False, lfs=False)
    local_spec = dict(common, source={'kind': 'local', 'path': str(source)},
                      branch={'kind': 'create', 'name': 'feature'})
    local_ready = clone(local_spec)
    assert git(destination, 'rev-parse', 'HEAD') == commit
    assert git(destination, 'symbolic-ref', '--short', 'HEAD') == 'feature'
    assert not (destination / 'dirty-untracked').exists()
    assert not (destination / '.git/objects/info/alternates').exists()
    git(destination, 'fsck', '--full')
    assert not git(destination, 'remote'), 'local source must not become a push remote'

    remote_destination = f.ROOT / 'remote-independent'
    remote_spec = dict(common, name='remote-independent',
                       source={'kind': 'remote', 'url': bare.resolve().as_uri()},
                       branch={'kind': 'detached'},
                       destination={'kind': 'custom', 'volume_uuid': volume['uuid'],
                                    'path': str(remote_destination)})
    remote_ready = clone(remote_spec)
    assert git(remote_destination, 'rev-parse', 'HEAD') == commit
    assert git(remote_destination, 'remote', 'get-url', 'origin') == bare.resolve().as_uri()
    assert git(bare, 'rev-parse', 'refs/heads/main') == commit
    assert not (remote_destination / '.git/objects/info/alternates').exists()

    # An acquired commit can reveal a submodule source absent from the initial
    # remote-ref review. Retain this exact stage until the trusted client sees
    # and approves the source. No second top-level acquisition is permitted.
    child = f.ROOT / 'child'
    child.mkdir()
    git(child, 'init', '-q', '-b', 'main')
    (child / 'nested.txt').write_text('CHILD COMMITTED DATA\n')
    git(child, 'add', 'nested.txt')
    git(child, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'child')
    child_bare = f.ROOT / 'child.git'
    subprocess.run(['git', 'clone', '--bare', '-q', str(child), str(child_bare)],
                   env=f.env, check=True, capture_output=True, timeout=30)
    git(source, '-c', 'protocol.file.allow=always', 'submodule', 'add', '-q',
        '../child.git', 'libs/child')
    git(source, 'add', '.gitmodules', 'libs/child')
    git(source, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'commit', '-qm', 'parent with submodule')
    trust_destination = f.ROOT / 'reviewed-checkout'
    trust_spec = dict(local_spec, name='reviewed-checkout', branch={'kind': 'detached'},
                      destination={'kind': 'custom', 'volume_uuid': volume['uuid'],
                                   'path': str(trust_destination)}, submodules=True)
    trust_review = rpc('review_clone', 'clone_review', expected_revision=status()['revision'], spec=trust_spec)
    trust_id = uid()
    trust_initial = rpc('begin_clone', 'clone', request=trust_id, review=trust_review['id'])
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        paused = rpc('inspect_clone', 'clone', request=trust_id)
        if paused['state'] in ('awaiting_trust', 'ready', 'preparation_failed', 'recovery_required'):
            break
        time.sleep(.1)
    assert paused['state'] == 'awaiting_trust' and not trust_destination.exists(), paused
    source_notice, = paused['pending_trust']
    assert source_notice['kind'] == 'submodule' and source_notice['url'] == '../child.git', paused
    assert source_notice['path'] == 'libs/child'
    stage = Path(paused['stage'])
    witness = (stage.stat().st_dev, stage.stat().st_ino)
    assert not (stage / 'libs/child/nested.txt').exists(), 'unreviewed transport was used'
    assert len(paused['output_runs']) == 1, paused
    assert rpc('resume_clone', 'clone', request=trust_id)['state'] == 'awaiting_trust'
    assert rpc('inspect_clone', 'clone', request=trust_id)['output_runs'] == paused['output_runs']
    existing = rpc('clone_output', 'clone_output', clone=trust_id,
                   request={'action': 'inspect', 'run_id': paused['output_runs'][0]})
    assert existing['kind'] == 'status' and existing['run']['state'] == 'failed' and existing['run']['complete'], existing
    source_review = rpc('review_clone_trust', 'clone_trust_review',
                        clone=trust_id, expected_revision=paused['revision'])
    assert source_review['sources'] == paused['pending_trust'] and source_review['stage'] == str(stage)
    approval_id = uid()
    approval = rpc('apply_clone_trust', 'receipt', request=approval_id, review=source_review['id'])
    assert not approval['issues'], approval
    assert rpc('apply_clone_trust', 'receipt', request=approval_id, review=source_review['id']) == approval
    pending = rpc('resume_clone', 'clone', request=trust_id)
    assert pending['location'] == trust_initial['location']
    trust_ready = await_clone(trust_id)
    assert trust_ready['location'] == trust_initial['location']
    assert len(trust_ready['trust_approvals']) == 1 and not trust_ready['pending_trust'], trust_ready
    assert len(trust_ready['output_runs']) == 2, trust_ready
    await_output(trust_id, trust_ready['output_runs'][-1])
    assert (trust_destination.stat().st_dev, trust_destination.stat().st_ino) == witness
    assert (trust_destination / 'libs/child/nested.txt').read_text() == 'CHILD COMMITTED DATA\n'
    git(trust_destination, 'fsck', '--full')
    git(trust_destination / 'libs/child', 'fsck', '--full')
    assert not (trust_destination / 'libs/child/.git/objects/info/alternates').exists()

    # Existing data adoption is an explicit organization operation, not cloning.
    standalone, = change('register_location', name='existing', path=str(source),
                         registration={'kind': 'standalone'})['targets']
    adopted, = change('adopt_standalone', location=standalone['id'], home=home,
                      repository=repo['id'], associate_repository=False)['targets']
    assert adopted['id'] == standalone['id']
    plain = f.ROOT / 'reference'
    plain.mkdir()
    (plain / 'sentinel').write_text('unchanged')
    change('register_location', name='reference', path=str(plain),
           registration={'kind': 'directory', 'home': home})
    assert (plain / 'sentinel').read_text() == 'unchanged'

    old = rpc('inspect', 'entity', target={'kind': 'location', 'id': local_ready['location']})['value']
    moved = f.ROOT / 'relocated-independent'
    os.rename(destination, moved)
    receipt = change('rebind_location', location=local_ready['location'],
                     expected_old_path=old['observed_path'],
                     expected_generation=old['binding_generation'], new_path=str(moved))
    history = rpc('inspect_rebind', 'rebind', operation=receipt['operation'])
    assert history['location'] == local_ready['location'] and history['new_generation'] == old['binding_generation'] + 1
    assert history['new_path'] == str(moved.resolve())
    assert not destination.exists()
    assert git(moved, 'rev-parse', 'HEAD') == commit
    # Different clone key does not inherit Startup Context. Copy is reviewed separately.
    (moved / 'PLAN.md').write_text('TARGET CONTEXT\n')
    copy_review = rpc('review_startup_copy', 'startup_copy_review',
                      expected_catalog_revision=status()['revision'], source=str(source),
                      target=local_ready['location'], expected_source_plan_revision=1,
                      expected_target_plan_revision=0, external_approvals=[])
    assert len(copy_review['entries']) == 1 and copy_review['entries'][0]['selected_path'] == 'PLAN.md'
    copy_id = uid()
    copied = rpc('apply_startup_copy', 'startup_copy', request=copy_id, review=copy_review['id'])
    assert copied['state'] == 'complete' and copied['issue'] is None, copied
    assert rpc('apply_startup_copy', 'startup_copy', request=copy_id, review=copy_review['id']) == copied
    assert rpc('inspect_startup_copy', 'startup_copy', request=copy_id) == copied
    request('subscribe', 'done', working_dir=str(moved), selfdev=False,
            agent='global:jcode', startup_context_caller='harness_api_create')
    snapshot = request('get_startup_context_status', 'startup_context_status')['snapshot']
    row, = snapshot['files']
    detail = request('get_startup_context_file_detail', 'startup_context_file_detail',
                     batch_id=row['batch_id'], spec_id=row['spec_id'], message_id=row['message_id'],
                     expected_sha256=row['sha256'])['detail']
    assert detail['content'] == 'TARGET CONTEXT\n'
    assert not f.posts, 'admin and Startup Context capture must make zero provider calls'
    result = dict(binary=subprocess.check_output([f.BIN, '--version'], text=True).strip(),
                  artifact=str(f.ROOT), checkout_version=probe['checkout_version'],
                  local_request=local_ready['request'], remote_request=remote_ready['request'],
                  paused_trust_request=trust_id, reviewed_resume_one_stage=True,
                  local_independent=True, remote_independent=True,
                  adopted_existing_identity=True, rebind_generation=history['new_generation'],
                  startup_copy_target_capture=detail['content'],
                  provider_requests=len(f.posts), managed_rollout=False)
    (f.ROOT / 'checkout-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
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
    (f.ROOT / 'events.json').write_text(json.dumps(f.events, indent=2))
    (f.ROOT / 'provider-posts.json').write_text(json.dumps(f.posts, indent=2))
    (f.ROOT / 'cleanup.json').write_text(json.dumps({
        'owned_daemon_terminal': f.proc is None or f.proc.poll() is not None,
        'fixture_checkout_preserved_for_audit': True,
    }, indent=2))
    print('artifacts=' + str(f.ROOT))
