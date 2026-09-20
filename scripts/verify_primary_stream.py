#!/usr/bin/env python3
"""Native bounded-observer disconnect and canonical reconnect, without paid inference."""
import os
if not os.environ.get('JCODE_TEST_STATE_ROOT'):
    raise SystemExit('Run through scripts/run_isolated_test.py')
import json, socket, tempfile, threading, time
from pathlib import Path
import test_instruction_manager as f
ipc = Path(tempfile.mkdtemp(prefix='ps-', dir=os.environ['JCODE_RUNTIME_DIR']))
old = str(f.sockpath)
f.sockpath = ipc / 's.sock'
f.args = [str(f.sockpath) if a == old else a for a in f.args]
f.env['JCODE_SOCKET'] = str(f.sockpath)
expected = 'native-stream-segment|' * 12000
sent = threading.Event()
peer = None
reader = None

def response(self):
    f.posts.append(self.rfile.read(int(self.headers.get('Content-Length', '0'))).decode())
    self.send_response(200)
    self.send_header('Content-Type', 'text/event-stream')
    self.end_headers()
    for text, stop in [('native-stream-segment|', None)] * 12000 + [('', 'stop')]:
        chunk = {'id': 'fixture', 'object': 'chat.completion.chunk', 'choices': [{'index': 0, 'delta': {'content': text}, 'finish_reason': stop}]}
        self.wfile.write(('data: ' + json.dumps(chunk) + '\n\n').encode())
    self.wfile.write(b'data: [DONE]\n\n')
    self.wfile.flush()
    sent.set()
f.FixtureProvider.do_POST = response
try:
    f.start()
    session = f.subscribe()
    f.send({'type': 'message', 'id': 900, 'content': 'Synthetic bounded observer fixture', 'images': []})
    assert sent.wait(60), 'fixture stream did not finish'
    deadline = time.monotonic() + 120
    complete = False
    reconnects = 0
    while time.monotonic() < deadline:
        try:
            if peer is None:
                peer = socket.socket(socket.AF_UNIX)
                peer.settimeout(120)
                peer.connect(str(f.sockpath))
                reader = peer.makefile('rb')
                reconnects += 1
                peer.sendall(b'{"type":"primary_stream_subscribe","id":903}\n')
                capability = json.loads(reader.readline())
                assert capability.get('type') == 'primary_stream_capabilities', capability
                peer.sendall((json.dumps({'type': 'subscribe', 'id': 901, 'target_session_id': session, 'working_dir': str(f.project), 'selfdev': False}) + '\n').encode())
            peer.sendall(b'{"type":"get_history","id":902}\n')
            while True:
                line = reader.readline()
                if not line:
                    raise ConnectionResetError('observer overflowed during active burst')
                event = json.loads(line)
                if event.get('type') == 'history' and event.get('id') == 902:
                    assert 'primary_stream' in event, event
                    text = ''.join((m['content'] for m in event['messages'] if m['role'] == 'assistant'))
                    if text == expected:
                        complete = True
                    break
            if complete:
                break
        except (ConnectionResetError, BrokenPipeError):
            if reader:
                reader.close()
            if peer:
                peer.close()
            reader = None
            peer = None
        time.sleep(0.05)
    assert complete, 'canonical output incomplete after observer loss'
    f.client.settimeout(10)
    received = 0
    while True:
        line = f.reader.readline()
        if not line:
            break
        received += 1
        assert received < 15000, 'original reader unexpectedly received an unbounded stream'
    assert len(f.posts) == 1
    result = {'binary': f.BIN, 'root': str(f.ROOT), 'session': session, 'original_connection_closed': True, 'canonical_bytes': len(expected.encode()), 'provider_calls': 1, 'buffered_frames': received, 'reconnect_attempts': reconnects}
    (f.ROOT / 'primary-stream-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
    if reader:
        reader.close()
    if peer:
        peer.close()
    if f.reader:
        f.reader.close()
    if f.client:
        f.client.close()
    if f.proc and f.proc.poll() is None:
        f.proc.terminate()
        try:
            f.proc.wait(timeout=30)
        except Exception:
            f.proc.kill()
            f.proc.wait(timeout=10)
    f.http.shutdown()
    f.http.server_close()
    f.log.close()
    (f.ROOT / 'primary-stream-cleanup.json').write_text(json.dumps({'daemon_exit': f.proc.returncode if f.proc else None}))
