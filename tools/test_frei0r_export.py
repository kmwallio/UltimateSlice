#!/usr/bin/env python3
"""Test every frei0r filter with FFmpeg directly.

Discovers all frei0r .so files, reads their native param types via ctypes,
builds the correct FFmpeg filter_params string (with default values), and
invokes FFmpeg to verify the filter chain works.
"""

import ctypes
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


# --- frei0r FFI structures ---
class F0rPluginInfo(ctypes.Structure):
    _fields_ = [
        ('name', ctypes.c_char_p),
        ('author', ctypes.c_char_p),
        ('plugin_type', ctypes.c_int),
        ('color_model', ctypes.c_int),
        ('frei0r_version', ctypes.c_int),
        ('major_version', ctypes.c_int),
        ('minor_version', ctypes.c_int),
        ('num_params', ctypes.c_int),
        ('explanation', ctypes.c_char_p),
    ]

class F0rParamInfo(ctypes.Structure):
    _fields_ = [
        ('name', ctypes.c_char_p),
        ('param_type', ctypes.c_int),
        ('explanation', ctypes.c_char_p),
    ]

TYPE_BOOL = 0
TYPE_DOUBLE = 1
TYPE_COLOR = 2
TYPE_POSITION = 3
TYPE_STRING = 4


def discover_frei0r_filters():
    """Discover all frei0r filter .so files and extract param info."""
    dirs = ['/usr/lib/frei0r-1', '/usr/local/lib/frei0r-1',
            os.path.expanduser('~/.frei0r-1/lib')]
    filters = []

    for d in dirs:
        if not os.path.isdir(d):
            continue
        for f in sorted(os.listdir(d)):
            if not f.endswith('.so'):
                continue
            so_name = f[:-3]
            path = os.path.join(d, f)
            try:
                lib = ctypes.CDLL(path)
                lib.f0r_init()
                info = F0rPluginInfo()
                lib.f0r_get_plugin_info(ctypes.byref(info))

                if info.plugin_type != 0:  # Only filters (type 0)
                    lib.f0r_deinit()
                    continue

                params = []
                for i in range(info.num_params):
                    p = F0rParamInfo()
                    lib.f0r_get_param_info(ctypes.byref(p), ctypes.c_int(i))
                    params.append({
                        'name': p.name.decode() if p.name else f'param{i}',
                        'type': p.param_type,
                    })
                lib.f0r_deinit()

                filters.append({
                    'so_name': so_name,
                    'display_name': info.name.decode() if info.name else so_name,
                    'params': params,
                })
            except Exception as e:
                filters.append({
                    'so_name': so_name,
                    'display_name': so_name,
                    'params': [],
                    'discover_error': str(e),
                })
    return filters


def build_default_params_str(params):
    """Build FFmpeg filter_params string with safe defaults."""
    parts = []
    for p in params:
        pt = p['type']
        if pt == TYPE_BOOL:
            parts.append('n')
        elif pt == TYPE_DOUBLE:
            parts.append('0.500000')
        elif pt == TYPE_COLOR:
            parts.append('0.500000/0.500000/0.500000')
        elif pt == TYPE_POSITION:
            parts.append('0.500000/0.500000')
        elif pt == TYPE_STRING:
            parts.append('')
        else:
            parts.append('0.500000')
    return '|'.join(parts)


def test_filter_with_ffmpeg(so_name, params_str, media_path, out_dir, timeout=30):
    """Run FFmpeg with the frei0r filter and return (success, error_msg)."""
    out_path = os.path.join(out_dir, f'{so_name}.mp4')

    if params_str:
        vf = f'frei0r=filter_name={so_name}:filter_params={params_str}'
    else:
        vf = f'frei0r=filter_name={so_name}'

    cmd = [
        'ffmpeg', '-y', '-hide_banner', '-loglevel', 'error',
        '-f', 'lavfi', '-i', 'testsrc=duration=0.5:size=320x240:rate=10',
        '-vf', vf,
        '-frames:v', '5',
        '-c:v', 'libx264', '-crf', '23',
        out_path,
    ]

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        # Clean up
        try:
            os.remove(out_path)
        except OSError:
            pass

        if result.returncode == 0:
            return True, ''
        else:
            return False, result.stderr.strip()[:200]
    except subprocess.TimeoutExpired:
        try:
            os.remove(out_path)
        except OSError:
            pass
        return False, f'timeout after {timeout}s'
    except Exception as e:
        return False, str(e)


def main():
    import argparse
    parser = argparse.ArgumentParser(description='Test frei0r filters with FFmpeg')
    parser.add_argument('--results', default='/tmp/frei0r_ffmpeg_test_results.json')
    parser.add_argument('--filter', default='', help='Test only this filter')
    parser.add_argument('--timeout', type=float, default=30)
    args = parser.parse_args()

    out_dir = tempfile.mkdtemp(prefix='frei0r_test_')

    print('=' * 60)
    print('Frei0r FFmpeg Export Test')
    print('=' * 60)
    print(f'  Output dir: {out_dir}')
    print()

    print('Discovering frei0r filters...')
    filters = discover_frei0r_filters()
    print(f'  Found {len(filters)} filters')

    if args.filter:
        filters = [f for f in filters if f['so_name'] == args.filter]
        if not filters:
            print(f'Filter {args.filter!r} not found')
            return 1

    total = len(filters)
    print(f'\nTesting {total} filters...\n')

    results = []
    passed = 0
    failed = 0

    for i, filt in enumerate(filters):
        name = filt['so_name']
        sys.stdout.write(f'  [{i+1:3d}/{total}] {name:40s} ')
        sys.stdout.flush()

        if 'discover_error' in filt:
            r = {'so_name': name, 'ok': False, 'error': f"discover: {filt['discover_error']}"}
        else:
            params_str = build_default_params_str(filt['params'])
            t0 = time.time()
            ok, err = test_filter_with_ffmpeg(name, params_str, None, out_dir, args.timeout)
            elapsed = time.time() - t0
            r = {'so_name': name, 'ok': ok, 'error': err, 'elapsed_s': round(elapsed, 1),
                 'params_str': params_str, 'param_count': len(filt['params'])}

        results.append(r)
        if r['ok']:
            passed += 1
            print(f'✓  ({r.get("elapsed_s", 0):.1f}s)')
        else:
            failed += 1
            print(f'✗  ({r.get("elapsed_s", 0):.1f}s) {r["error"][:80]}')

    print('\n' + '=' * 60)
    print(f'Results: {passed} passed, {failed} failed, {total} total')
    print('=' * 60)

    if failed:
        print('\nFailed:')
        for r in results:
            if not r['ok']:
                print(f'  - {r["so_name"]}: {r["error"][:120]}')

    report = {'total': total, 'passed': passed, 'failed': failed, 'results': results}
    with open(args.results, 'w') as f:
        json.dump(report, f, indent=2)
    print(f'\nReport: {args.results}')

    # Cleanup
    try:
        os.rmdir(out_dir)
    except OSError:
        pass

    return 0 if failed == 0 else 1

if __name__ == '__main__':
    raise SystemExit(main())
