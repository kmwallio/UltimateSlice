#!/usr/bin/env python3
"""MCP baseline test for multi-track preview rendering.

Connects to a running UltimateSlice instance via --mcp-attach,
opens the three-video-tracks project, seeks to various positions,
and exports frames to detect frozen/identical frame bugs.
"""
import subprocess
import json
import time
import os
import sys
import hashlib

CWD = os.path.dirname(os.path.abspath(__file__))
SERVER_CMD = [os.path.join(CWD, "target/release/ultimate-slice"), "--mcp"]
FCPXML = os.path.join(CWD, "Sample-Media/three-video-tracks.fcpxml")
FRAME_DIR = "/tmp/ultimateslice-mcp-frames"

# Timeline regions (nanoseconds):
# V1 (GX010429): 0s - 31.625s   (track 0)
# V2 (GX010430): 5s - 15.5s     (track 1)
# V3 (Screencast): 9.167s - 13.458s (track 3)
# 1-track region: 0-5s (V1 only)
# 2-track region: 5-9.167s (V1 + V2)
# 3-track region: 9.167s - 13.458s (V1 + V2 + V3)

POSITIONS = {
    "1track_2s":  2_000_000_000,      # 2s - V1 only
    "1track_4s":  4_000_000_000,      # 4s - V1 only
    "2track_7s":  7_000_000_000,      # 7s - V1 + V2
    "2track_8s":  8_000_000_000,      # 8s - V1 + V2
    "3track_10s": 10_000_000_000,     # 10s - all three
    "3track_11s": 11_000_000_000,     # 11s - all three
    "3track_12s": 12_000_000_000,     # 12s - all three
    "3track_13s": 13_000_000_000,     # 13s - all three
}


def send(proc, request):
    req_json = json.dumps(request)
    proc.stdin.write(req_json + "\n")
    proc.stdin.flush()


def read_response(proc, timeout=30):
    import select
    start = time.time()
    while time.time() - start < timeout:
        if select.select([proc.stdout], [], [], 1.0)[0]:
            line = proc.stdout.readline()
            if line:
                return json.loads(line)
    return None


def call_tool(proc, req_id, name, args, timeout=30):
    send(proc, {
        "jsonrpc": "2.0",
        "id": req_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
    return read_response(proc, timeout=timeout)


def file_hash(path):
    """Return MD5 hash of a file's contents."""
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    os.makedirs(FRAME_DIR, exist_ok=True)
    # Clean old frames
    for f in os.listdir(FRAME_DIR):
        if f.endswith(".ppm"):
            os.remove(os.path.join(FRAME_DIR, f))

    print(f"Starting MCP attach: {' '.join(SERVER_CMD)}", flush=True)
    env = os.environ.copy()
    env["RUST_LOG"] = "ultimate_slice=info"
    proc = subprocess.Popen(
        SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open("/tmp/us-mcp-debug.log", "w"),
        text=True,
        bufsize=1,
        env=env,
    )

    # Initialize
    send(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "perf-test", "version": "1"}
    }})
    resp = read_response(proc)
    if resp is None:
        print("ERROR: No response to initialize. Is the app running with MCP socket enabled?")
        proc.terminate()
        sys.exit(1)
    print(f"Initialize: OK (server={resp.get('result', {}).get('serverInfo', {})})", flush=True)

    # Open the test project
    print(f"\nOpening project: {FCPXML}", flush=True)
    resp = call_tool(proc, 2, "open_fcpxml", {"path": FCPXML})
    print(f"Open: {extract_text(resp)}", flush=True)
    time.sleep(1)  # Let pipeline settle

    # Get project info
    resp = call_tool(proc, 3, "get_project", {})
    print(f"Project: {extract_text(resp)[:200]}", flush=True)

    # List tracks
    resp = call_tool(proc, 4, "list_tracks", {})
    print(f"Tracks: {extract_text(resp)}", flush=True)

    # Run tests with different configurations
    configs = [
        {"name": "default", "quality": None, "priority": None},
        {"name": "smooth", "quality": None, "priority": "smooth"},
        {"name": "half_quality", "quality": "half", "priority": "smooth"},
        {"name": "quarter_quality", "quality": "quarter", "priority": "smooth"},
    ]

    req_id = 10
    results = {}

    for config in configs:
        cname = config["name"]
        print(f"\n{'='*60}")
        print(f"Testing config: {cname}")
        print(f"{'='*60}")

        # Apply config
        if config["priority"]:
            req_id += 1
            resp = call_tool(proc, req_id, "set_playback_priority", {"priority": config["priority"]})
            print(f"  Set priority={config['priority']}: {extract_text(resp)}", flush=True)

        if config["quality"]:
            req_id += 1
            resp = call_tool(proc, req_id, "set_preview_quality", {"quality": config["quality"]})
            print(f"  Set quality={config['quality']}: {extract_text(resp)}", flush=True)
            time.sleep(0.5)

        hashes = {}
        for label, pos_ns in POSITIONS.items():
            req_id += 1
            resp = call_tool(proc, req_id, "seek_playhead", {"timeline_pos_ns": pos_ns})
            time.sleep(0.5)  # Let compositor settle

            frame_path = os.path.join(FRAME_DIR, f"{cname}_{label}.ppm")
            req_id += 1
            resp = call_tool(proc, req_id, "export_displayed_frame", {"path": frame_path})
            status = extract_text(resp)

            if os.path.exists(frame_path):
                h = file_hash(frame_path)
                size = os.path.getsize(frame_path)
                hashes[label] = h
                print(f"  {label} @ {pos_ns/1e9:.1f}s: OK (hash={h[:12]}... size={size})", flush=True)
            else:
                hashes[label] = None
                print(f"  {label} @ {pos_ns/1e9:.1f}s: FAILED ({status})", flush=True)

        results[cname] = hashes

        # Analyze: check for frozen frames
        print(f"\n  Frame analysis for {cname}:")
        # Check 3-track frames are distinct from each other
        three_track_hashes = [hashes.get(k) for k in ["3track_10s", "3track_11s", "3track_12s", "3track_13s"] if hashes.get(k)]
        unique_3track = len(set(three_track_hashes))
        total_3track = len(three_track_hashes)
        if total_3track > 0:
            if unique_3track == 1:
                print(f"  ⚠️  FROZEN: All {total_3track} 3-track frames are IDENTICAL")
            elif unique_3track < total_3track:
                print(f"  ⚠️  PARTIAL FREEZE: {unique_3track}/{total_3track} unique 3-track frames")
            else:
                print(f"  ✅ All {total_3track} 3-track frames are unique")

        # Check 1-track and 2-track frames are distinct
        other_hashes = [hashes.get(k) for k in ["1track_2s", "1track_4s", "2track_7s", "2track_8s"] if hashes.get(k)]
        unique_other = len(set(other_hashes))
        total_other = len(other_hashes)
        if total_other > 0:
            if unique_other == total_other:
                print(f"  ✅ All {total_other} 1/2-track frames are unique")
            else:
                print(f"  ⚠️  {unique_other}/{total_other} unique 1/2-track frames")

        # Check 3-track frames differ from 1-track frames
        if hashes.get("1track_2s") and hashes.get("3track_10s"):
            if hashes["1track_2s"] == hashes["3track_10s"]:
                print(f"  ⚠️  3-track frame identical to 1-track frame (stuck on same content)")
            else:
                print(f"  ✅ 3-track frames differ from 1-track frames")

    # Reset quality to full
    req_id += 1
    call_tool(proc, req_id, "set_preview_quality", {"quality": "full"})

    # Summary
    print(f"\n{'='*60}")
    print("SUMMARY")
    print(f"{'='*60}")
    for cname, hashes in results.items():
        three_track = [hashes.get(k) for k in ["3track_10s", "3track_11s", "3track_12s", "3track_13s"] if hashes.get(k)]
        unique = len(set(three_track))
        total = len(three_track)
        status = "✅ OK" if unique == total and total > 0 else f"⚠️  FROZEN ({unique}/{total} unique)" if total > 0 else "❌ NO FRAMES"
        print(f"  {cname}: {status}")

    proc.terminate()
    try:
        proc.communicate(timeout=5)
    except Exception:
        pass

    print(f"\nFrames saved in: {FRAME_DIR}")


def extract_text(resp):
    """Extract text content from MCP response."""
    if resp is None:
        return "NO RESPONSE"
    if "error" in resp:
        return f"ERROR: {resp['error']}"
    result = resp.get("result", {})
    content = result.get("content", [])
    if content:
        return content[0].get("text", str(result))
    return str(result)


if __name__ == "__main__":
    main()
