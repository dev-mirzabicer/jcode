#!/usr/bin/env python3
"""Regression checks for test processes leaking into a live Jcode home."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name("run_isolated_test.py")


class TestIsolation(unittest.TestCase):
    def invoke(self, code, **overrides):
        return subprocess.run([sys.executable, str(RUNNER), sys.executable, "-c", code],
                              env=dict(os.environ, **overrides), text=True, capture_output=True)

    def test_all_state_paths_and_home_fallback_are_private(self):
        code = '''
import json, os
from pathlib import Path
root = Path(os.environ['JCODE_TEST_STATE_ROOT'])
for key in ['HOME','JCODE_HOME','JCODE_RUNTIME_DIR','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR','TMPDIR']:
    assert Path(os.environ[key]).is_relative_to(root), key
assert 'JCODE_SOCKET' not in os.environ
assert 'JCODE_DEBUG_SOCKET' not in os.environ
assert 'JCODE_API_SOCKET' not in os.environ
assert 'JCODE_REPO_DIR' not in os.environ
assert Path(os.environ['CARGO_HOME']).is_absolute()
assert Path(os.environ['RUSTUP_HOME']).is_absolute()
os.environ.pop('JCODE_HOME')
fallback = Path.home()/'.jcode/instructions/instruction-store.toml'
fallback.parent.mkdir(parents=True)
fallback.write_text('seed_version = 999')
assert fallback.is_relative_to(root)
print(json.dumps({'root': str(root), 'fallback': str(fallback)}))
'''
        first = self.invoke(code, JCODE_HOME="/must-not-use", JCODE_SOCKET="/live.sock",
                            JCODE_DEBUG_SOCKET="/debug.sock", JCODE_API_SOCKET="/live-api.sock", JCODE_REPO_DIR="/live/repo")
        self.assertEqual(first.returncode, 0, first.stderr)
        second = self.invoke(code)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertNotEqual(json.loads(first.stdout)['root'], json.loads(second.stdout)['root'])

    def test_failed_test_status_and_evidence_are_retained(self):
        result = self.invoke("import os,sys; print(os.environ['JCODE_TEST_STATE_ROOT']); sys.exit(19)")
        self.assertEqual(result.returncode, 19)
        evidence = json.loads((Path(result.stdout.strip())/"result.json").read_text())
        self.assertEqual(evidence["returncode"], 19)

    def test_dev_cargo_wraps_tests_and_preserves_non_test_home(self):
        with tempfile.TemporaryDirectory(prefix="jcode-wrapper-check-") as directory:
            root = Path(directory)
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "probe-home=%s\\n" "$HOME"\nprintf "running 1 tests\\n"\n')
            cargo.chmod(0o700)
            env = dict(os.environ, PATH=str(root)+os.pathsep+os.environ['PATH'],
                       JCODE_HOME=str(root/'state'), JCODE_RUNTIME_DIR=str(root/'runtime'),
                       JCODE_REMOTE_CARGO='0', JCODE_RUST_ACTION_LOG='0', JCODE_CARGO_GATE='off',
                       JCODE_BUILD_JOBS='1', RUSTC_WRAPPER='', TMPDIR=str(root))
            for action in ['test', 'check', 'build']:
                result = subprocess.run(['/bin/bash' if Path('/bin/bash').exists() else 'bash',str(RUNNER.with_name('dev_cargo.sh')),action],
                                        env=env,text=True,capture_output=True)
                self.assertEqual(result.returncode,0,result.stderr)
                probe = next(line.split('=',1)[1] for line in result.stdout.splitlines() if line.startswith('probe-home='))
                if action == 'test':
                    self.assertNotEqual(probe,str(Path.home()))
                    self.assertIn('test state:',result.stderr)
                else:
                    self.assertEqual(probe,str(Path.home()))

    def test_remote_test_command_includes_isolation_runner(self):
        with tempfile.TemporaryDirectory(prefix="jcode-remote-wrapper-") as directory:
            root=Path(directory)
            ssh=root/'ssh'
            ssh.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            ssh.chmod(0o700)
            env=dict(os.environ,JCODE_REMOTE_SSH_BIN=str(ssh),JCODE_REMOTE_HOST='fixture',
                     JCODE_REMOTE_CONFIG=str(root/'absent-config'))
            result=subprocess.run(['bash',str(RUNNER.with_name('remote_build.sh')),
                                   '--no-sync','--no-sync-back','test','isolated_fixture'],
                                  env=env,text=True,capture_output=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertIn('run_isolated_test.py',result.stdout)


if __name__ == "__main__":
    unittest.main()
