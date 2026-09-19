#!/usr/bin/env python3
"""Workspace catalog acceptance through an actual isolated daemon, without inference.

Run with run_isolated_test.py and an explicit --binary. Only owned fixture roots
and catalog data are changed. No real checkout, active catalog or service is used.
"""
import json
import os
import socket
import subprocess
import uuid

import test_instruction_manager as f

if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')


def uid():
    return str(uuid.uuid4())


def command(action, expected=None, **fields):
    f.counter += 1
    request_id = f.counter
    f.send({'type': 'workspace', 'id': request_id,
            'request': {'action': action, **fields}})
    event = f.until(lambda e: e.get('id') == request_id and
                    e.get('type') in ('workspace_response', 'error'))
    assert event['type'] == 'workspace_response', event
    response = event['response']
    if expected is not None:
        assert response['kind'] == expected, response
    return response['value']


def status():
    return command('status', 'status')


def change(action, **fields):
    review = command('review', 'review', expected_revision=status()['revision'],
                     change={'action': action, **fields})
    request = uid()
    receipt = command('apply', 'receipt', request=request, review=review['id'])
    assert not receipt['issues'], receipt
    assert command('apply', 'receipt', request=request, review=review['id']) == receipt
    return receipt['targets']


def listing(**fields):
    query = dict(kind=None, project=None, home=None, repository=None,
                 visibility='all', active_sessions_only=False)
    query.update(fields)
    rows = []
    after = None
    while True:
        page = command('list', 'page', query=query, after=after, limit=2)
        rows.extend(page['items'])
        after = page['next']
        if after is None:
            assert len(rows) == page['total']
            return rows


def reconnect():
    f.reader.close()
    f.client.close()
    f.client = socket.socket(socket.AF_UNIX)
    f.client.settimeout(30)
    f.client.connect(str(f.sockpath))
    f.reader = f.client.makefile('rb')


result = {}
try:
    f.start()
    f.counter += 1
    f.send({'type': 'workspace_probe', 'id': f.counter})
    probe = f.until(lambda e: e.get('id') == f.counter)
    assert probe['type'] == 'workspace_capabilities' and probe['catalog_version'] == 1
    assert probe['managed_rollout'] is False
    assert command('status', 'error')['code'] in ('corrupt_state', 'recovery_required')
    initial = command('initialize', 'status', request=uid())
    assert not initial['managed_rollout']
    project, = change('create_project', name='Fixture project')
    other, = change('create_project', name='Fixture project')
    assert project != other
    repo, = change('create_repository', name='Repo one', remotes=['https://example.invalid/repo.git'])
    repo2, = change('create_repository', name='Repo two', remotes=['https://example.invalid/repo.git'])
    assert repo != repo2
    for repository in (repo, repo2):
        change('associate_repository', project=project['id'], repository=repository['id'])
    area, = change('create_work_area', project=project['id'], name='Area')
    home = {'kind': 'work_area', 'id': area['id']}
    gitroot = f.ROOT / 'checkout'
    subprocess.run(['git', 'init', '--quiet', str(gitroot)], check=True, env=f.env)
    sentinel = gitroot / 'sentinel'
    sentinel.write_text('Owned source must remain unchanged\n')
    checkout, = change('register_location', name='Checkout', path=str(gitroot),
                       registration={'kind': 'checkout', 'home': home, 'repository': repo['id']})
    alias = f.ROOT / 'checkout-alias'
    alias.symlink_to(gitroot, target_is_directory=True)
    assert change('register_location', name='Alias', path=str(alias),
                  registration={'kind': 'standalone'}) == [checkout]
    plain = f.ROOT / 'plain'
    plain.mkdir()
    directory, = change('register_location', name='Directory', path=str(plain),
                        registration={'kind': 'directory', 'home': home})
    assert len(listing(kind='location', project=project['id'])) == 2
    change('archive', target=project, archived=True)
    assert not listing(kind='location', project=project['id'], visibility='current')
    change('archive', target=project, archived=False)
    assert len(listing(kind='location', project=project['id'], visibility='current')) == 2
    saved = command('backup', 'snapshot', request=uid(), name='Review boundary')
    change('rename', target=project, name='Later name')
    review = command('review_restore', 'restore_review', snapshot=saved['id'])
    command('apply_restore', 'receipt', request=uid(), review=review['id'])
    assert command('inspect', 'entity', target=project)['value']['name'] == 'Fixture project'
    export = command('export', 'export', request=uid(), project=project['id'], name='Portable fixture')
    remaproot = f.ROOT / 'import-checkout'
    subprocess.run(['git', 'init', '--quiet', str(remaproot)], check=True, env=f.env)
    remapdir = f.ROOT / 'import-directory'
    remapdir.mkdir()
    imported = command('review_import', 'import_review', path=export,
                       expected_revision=status()['revision'], collisions='new_identities',
                       remap=[{'location': checkout['id'], 'path': str(remaproot)},
                              {'location': directory['id'], 'path': str(remapdir)}])
    assert imported['collisions'] and not imported['issues']
    command('apply_import', 'receipt', request=uid(), review=imported['id'])
    assert len(listing(kind='project')) == 3
    before = status()
    reconnect()
    assert status() == before
    assert sentinel.read_text() == 'Owned source must remain unchanged\n'
    assert not f.posts
    assert not list((f.home / 'sessions').glob('*.json'))
    f.reader.close()
    f.client.close()
    f.client = None
    f.proc.terminate()
    f.proc.wait(timeout=15)
    f.start()
    assert status() == before
    assert len(listing(kind='location')) == 4
    assert not f.posts
    result = dict(binary=subprocess.check_output([f.BIN, '--version'], text=True).strip(),
                  provider_requests=0, provisional_sessions=0, managed_rollout=False,
                  organization_and_aliases=True, archive_roundtrip=True, backup_restore=True,
                  reviewed_portable_remap=True, reconnect_and_restart=True,
                  source_preserved=True, artifact=str(f.ROOT))
    (f.ROOT / 'workspace-result.json').write_text(json.dumps(result, indent=2))
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
    (f.ROOT / 'cleanup.json').write_text(json.dumps({'owned_daemon_terminal': f.proc is None or f.proc.poll() is not None}))
    print('artifacts=' + str(f.ROOT))
