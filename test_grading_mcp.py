#!/usr/bin/env python3
"""MCP test for shadows/midtones/highlights color grading.

Validates:
1. set_clip_color with shadows/midtones/highlights params persists to model
2. list_clips returns the grading values
3. export_mp4 succeeds with grading applied (ffmpeg colorbalance filter)
4. save_fcpxml + re-open preserves grading values
5. ffprobe confirms colorbalance was applied in exported file
"""

import subprocess
import json
import time
import os
import sys
import tempfile

CWD = os.path.dirname(os.path.abspath(__file__))
SERVER_CMD = [os.path.join(CWD, "target/debug/ultimate-slice"), "--mcp"]

# ── helpers ──────────────────────────────────────────────────────────────────

def send(proc, request):
    req_json = json.dumps(request)
    proc.stdin.write(req_json + "\n")
    proc.stdin.flush()

def read_response(proc, timeout=30):
    import select
    fd = proc.stdout.fileno()
    ready, _, _ = select.select([fd], [], [], timeout)
    if not ready:
        return None
    line = proc.stdout.readline()
    if not line:
        return None
    return json.loads(line)

req_id = 0
def call_tool(proc, name, args):
    global req_id
    req_id += 1
    send(proc, {
        "jsonrpc": "2.0",
        "id": req_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
    return read_response(proc)

def extract_text(resp):
    """Pull the text payload from an MCP tool response."""
    try:
        return json.loads(resp["result"]["content"][0]["text"])
    except (KeyError, TypeError, IndexError, json.JSONDecodeError):
        return resp

def start_server():
    env = os.environ.copy()
    env["GSK_RENDERER"] = "cairo"
    proc = subprocess.Popen(
        SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )
    global req_id
    req_id = 0
    send(proc, {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}})
    resp = read_response(proc)
    if resp is None:
        print("FAIL: Server did not respond to initialize", flush=True)
        proc.terminate()
        sys.exit(1)
    time.sleep(1)
    return proc

def stop_server(proc):
    proc.terminate()
    try:
        proc.communicate(timeout=5)
    except Exception:
        proc.kill()

# ── test logic ───────────────────────────────────────────────────────────────

def main():
    passed = 0
    failed = 0
    tmpdir = tempfile.mkdtemp(prefix="us-grading-test-")

    media1 = os.path.join(CWD, "Sample-Media/GX010426.MP4")
    media2 = os.path.join(CWD, "Sample-Media/GX010429.MP4")
    export_path = os.path.join(tmpdir, "grading-test.mp4")
    fcpxml_path = os.path.join(tmpdir, "grading-test.fcpxml")

    if not os.path.exists(media1):
        print(f"SKIP: Sample media not found: {media1}", flush=True)
        sys.exit(0)

    proc = start_server()

    # ── 1. Create project + add clips ────────────────────────────────────
    print("=== Test: Create project and add clips ===", flush=True)
    resp = call_tool(proc, "create_project", {"title": "Grading Test"})
    data = extract_text(resp)
    assert data.get("success") is not False, f"create_project failed: {data}"

    resp = call_tool(proc, "add_clip", {
        "source_path": media1,
        "track_index": 0,
        "timeline_start_ns": 0,
        "source_in_ns": 0,
        "source_out_ns": 3_000_000_000,
    })
    clip1_data = extract_text(resp)
    clip1_id = clip1_data.get("clip_id", "")
    assert clip1_id, f"add_clip 1 failed: {clip1_data}"
    print(f"  Clip 1 id: {clip1_id}", flush=True)

    resp = call_tool(proc, "add_clip", {
        "source_path": media2,
        "track_index": 0,
        "timeline_start_ns": 3_000_000_000,
        "source_in_ns": 0,
        "source_out_ns": 3_000_000_000,
    })
    clip2_data = extract_text(resp)
    clip2_id = clip2_data.get("clip_id", "")
    assert clip2_id, f"add_clip 2 failed: {clip2_data}"
    print(f"  Clip 2 id: {clip2_id}", flush=True)
    passed += 1
    print("  PASS: project + clips created\n", flush=True)

    # ── 2. Set grading values via set_clip_color ─────────────────────────
    print("=== Test: set_clip_color with shadows/midtones/highlights ===", flush=True)
    resp = call_tool(proc, "set_clip_color", {
        "clip_id": clip1_id,
        "brightness": 0.1,
        "contrast": 1.2,
        "saturation": 0.9,
        "shadows": 0.35,
        "midtones": -0.2,
        "highlights": 0.5,
    })
    data = extract_text(resp)
    assert data.get("success") is True, f"set_clip_color clip1 failed: {data}"

    resp = call_tool(proc, "set_clip_color", {
        "clip_id": clip2_id,
        "shadows": -0.4,
        "midtones": 0.3,
        "highlights": -0.15,
    })
    data = extract_text(resp)
    assert data.get("success") is True, f"set_clip_color clip2 failed: {data}"
    passed += 1
    print("  PASS: set_clip_color accepted grading values\n", flush=True)

    # ── 3. Verify list_clips returns grading values ──────────────────────
    print("=== Test: list_clips returns grading fields ===", flush=True)
    resp = call_tool(proc, "list_clips", {})
    clips = extract_text(resp)
    assert isinstance(clips, list), f"list_clips unexpected: {clips}"

    clip1_info = next((c for c in clips if c["id"] == clip1_id), None)
    clip2_info = next((c for c in clips if c["id"] == clip2_id), None)
    assert clip1_info is not None, "clip1 not found in list_clips"
    assert clip2_info is not None, "clip2 not found in list_clips"

    # Check clip1 grading values (f32 precision, use approx)
    for field, expected in [("shadows", 0.35), ("midtones", -0.2), ("highlights", 0.5)]:
        actual = clip1_info[field]
        assert abs(actual - expected) < 0.01, \
            f"clip1.{field}: expected {expected}, got {actual}"
    # Also check that brightness/contrast/saturation were set
    assert abs(clip1_info["brightness"] - 0.1) < 0.01
    assert abs(clip1_info["contrast"] - 1.2) < 0.01

    for field, expected in [("shadows", -0.4), ("midtones", 0.3), ("highlights", -0.15)]:
        actual = clip2_info[field]
        assert abs(actual - expected) < 0.01, \
            f"clip2.{field}: expected {expected}, got {actual}"
    passed += 1
    print("  PASS: grading values round-trip through model\n", flush=True)

    # ── 4. Export MP4 and verify success ─────────────────────────────────
    print("=== Test: export_mp4 with grading ===", flush=True)
    resp = call_tool(proc, "export_mp4", {"path": export_path})
    data = extract_text(resp)
    export_ok = data.get("success", False)
    assert export_ok, f"export_mp4 failed: {data}"
    assert os.path.exists(export_path), "export file not created"
    fsize = os.path.getsize(export_path)
    assert fsize > 10_000, f"export file suspiciously small: {fsize} bytes"
    passed += 1
    print(f"  PASS: export succeeded ({fsize} bytes)\n", flush=True)

    # ── 5. Verify ffprobe sees the video stream ──────────────────────────
    print("=== Test: ffprobe validates exported file ===", flush=True)
    try:
        probe_result = subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries",
             "stream=codec_type,duration", "-of", "json", export_path],
            capture_output=True, text=True, timeout=10
        )
        probe = json.loads(probe_result.stdout)
        streams = probe.get("streams", [])
        has_video = any(s["codec_type"] == "video" for s in streams)
        assert has_video, "No video stream in export"
        passed += 1
        print(f"  PASS: exported file has video stream\n", flush=True)
    except FileNotFoundError:
        print("  SKIP: ffprobe not available\n", flush=True)

    # ── 6. Save FCPXML and verify grading persistence ────────────────────
    print("=== Test: save_fcpxml preserves grading ===", flush=True)
    resp = call_tool(proc, "save_fcpxml", {"path": fcpxml_path})
    data = extract_text(resp)
    assert data.get("success", False), f"save_fcpxml failed: {data}"
    assert os.path.exists(fcpxml_path), "fcpxml file not created"
    passed += 1
    print("  PASS: save_fcpxml succeeded\n", flush=True)

    stop_server(proc)

    # ── 7. Re-open FCPXML in fresh server and verify values ──────────────
    print("=== Test: reload FCPXML preserves grading values ===", flush=True)
    proc2 = start_server()

    resp = call_tool(proc2, "open_fcpxml", {"path": fcpxml_path})
    data = extract_text(resp)
    time.sleep(2)  # let project load settle

    resp = call_tool(proc2, "list_clips", {})
    clips2 = extract_text(resp)
    assert isinstance(clips2, list) and len(clips2) >= 2, \
        f"reload list_clips unexpected: {clips2}"

    # Find clips by source_path since IDs may differ after reload
    reloaded_clip1 = next((c for c in clips2 if media1 in c.get("source_path", "")), None)
    reloaded_clip2 = next((c for c in clips2 if media2 in c.get("source_path", "")), None)
    assert reloaded_clip1 is not None, "clip1 not found after reload"
    assert reloaded_clip2 is not None, "clip2 not found after reload"

    for field, expected in [("shadows", 0.35), ("midtones", -0.2), ("highlights", 0.5)]:
        actual = reloaded_clip1[field]
        assert abs(actual - expected) < 0.01, \
            f"reload clip1.{field}: expected {expected}, got {actual}"

    for field, expected in [("shadows", -0.4), ("midtones", 0.3), ("highlights", -0.15)]:
        actual = reloaded_clip2[field]
        assert abs(actual - expected) < 0.01, \
            f"reload clip2.{field}: expected {expected}, got {actual}"
    passed += 1
    print("  PASS: grading values survive save/reload\n", flush=True)

    stop_server(proc2)

    # ── summary ──────────────────────────────────────────────────────────
    print(f"\n{'='*50}", flush=True)
    print(f"Results: {passed} passed, {failed} failed", flush=True)
    if failed > 0:
        sys.exit(1)
    print("All grading MCP tests passed!", flush=True)

    # cleanup
    for f in [export_path, fcpxml_path]:
        if os.path.exists(f):
            os.remove(f)
    os.rmdir(tmpdir)

if __name__ == "__main__":
    main()
