#!/usr/bin/env python3
"""Measure stack frames of the shared daemon's client path in a built binary.

The `selfdev` profile builds at `opt-level = 0`, where large async functions
keep big stack frames. Tokio runs client tasks on worker threads whose stack
size is set in `src/main.rs` (`RUNTIME_THREAD_STACK_BYTES`). This tool reads
each frame's size from the function prologue (`sub sp` and the `sub x9, sp`
stack-probe loop on arm64) with `objdump`. It does not run the binary.

Two modes:

  --report CRASH.ips   Sum the exact frames of the crashed thread in a macOS
                       crash report, measured in each given binary. Use this to
                       compare builds against an observed overflow.
  (default)            List the largest frames among client-path functions and
                       sum the largest frame of each known attach-chain stage.
                       This is an estimate of one deep path, not a proof that
                       no deeper path exists.

Examples:
  scripts/measure_client_stack.py ~/.jcode/builds/current/jcode
  scripts/measure_client_stack.py --report crash.ips old/jcode new/jcode
"""
import argparse, json, re, subprocess, sys

# Attach-chain stages observed in the October 2026 daemon overflow, outermost
# first. Each is matched against mangled symbol names.
CHAIN = [
    ('spawn_client_task', r'13ServerRuntime17spawn_client_task'),
    ('run_client_stream', r'13ServerRuntime17run_client_stream'),
    ('handle_client', r'43handle_client_with_instruction_repositories'),
    ('handle_subscribe', r'14client_session16handle_subscribe'),
]
CLIENT_PATH = r'jcode_app_core6server(16client_lifecycle|14client_session|7runtime|9live_turn|8primary|7primary)'


def prologue_bytes(lines):
    total = 0
    for line in lines[:40]:
        m = re.search(r'sub\s+x9, sp, #0x([0-9a-f]+), lsl #12', line)
        if m:
            total += int(m.group(1), 16) * 4096
            continue
        m = re.search(r'sub\s+sp, sp, #0x([0-9a-f]+)(, lsl #12)?', line)
        if m:
            if m.group(2) and total:
                continue  # the probe loop already counted these pages
            total += int(m.group(1), 16) * (4096 if m.group(2) else 1)
    return total


def frame_sizes(binary, names, batch=40):
    """Frame size per mangled symbol, disassembling in batches."""
    sizes = {}
    names = list(dict.fromkeys(names))
    for start in range(0, len(names), batch):
        chunk = names[start:start + batch]
        out = subprocess.run(['objdump', '--disassemble-symbols=' + ','.join(chunk), binary],
                             capture_output=True, text=True).stdout.splitlines()
        current, body = None, []
        for line in out + ['']:
            header = re.match(r'^[0-9a-f]+ <(.+)>:$', line)
            if header or not line:
                if current is not None and current not in sizes:
                    sizes[current] = prologue_bytes(body)
                current, body = (header.group(1), []) if header else (None, [])
            elif current is not None:
                body.append(line)
    return {name: sizes.get(name, 0) for name in names}


def symbols(binary):
    out = subprocess.run(['nm', '-U', binary], capture_output=True, text=True).stdout
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[1] in 'tT':
            yield parts[2]


def crash_frames(report):
    data = json.loads(open(report).read().split('\n', 1)[1])
    thread = [t for t in data['threads'] if t.get('triggered')][0]
    return ['_' + f['symbol'] for f in thread['frames'] if f.get('symbol', '').startswith('_R')]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--report', help='macOS crash report (.ips) of a stack overflow')
    parser.add_argument('--top', type=int, default=12)
    parser.add_argument('binaries', nargs='+')
    args = parser.parse_args()
    result = {}
    if args.report:
        frames = crash_frames(args.report)
        for binary in args.binaries:
            sizes = frame_sizes(binary, frames)
            result[binary] = {'crash_path_bytes': sum(sizes[s] for s in frames), 'frames': len(frames)}
    else:
        for binary in args.binaries:
            names = [s for s in symbols(binary) if re.search(CLIENT_PATH, s)]
            measured = frame_sizes(binary, names)
            sizes = sorted(((size, name) for name, size in measured.items()), reverse=True)
            chain = {}
            for stage, pattern in CHAIN:
                matches = [size for size, name in sizes if re.search(pattern, name)]
                chain[stage] = max(matches, default=0)
            result[binary] = {
                'client_path_functions': len(names),
                'largest': [{'bytes': size, 'symbol': name} for size, name in sizes[:args.top]],
                'attach_chain_largest_frames': chain,
                'attach_chain_estimate_bytes': sum(chain.values()),
            }
    json.dump(result, sys.stdout, indent=1)
    print()


if __name__ == '__main__':
    main()
