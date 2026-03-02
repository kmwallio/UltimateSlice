#!/usr/bin/env python3
"""Drive UltimateSlice via MCP to capture 10 feature screenshots.

Screenshots are saved to the cwd (repo root) with timestamped filenames.
Run from the repo root: python3 take_release_screenshots.py
"""

import json
import os
import select
import subprocess
import sys
import time

CWD = os.path.dirname(os.path.abspath(__file__))
SERVER_CMD = [os.path.join(CWD, "target/debug/ultimate-slice"), "--mcp"]
MEDIA_DIR = os.path.join(CWD, "Release-Media")

# Release-Media clips (shortest-first for a snappy demo timeline)
CLIPS = [
    "GX010088.MP4",   # 4.2 s
    "GX010077.MP4",   # 6.8 s
    "GX010100.MP4",   # 6.2 s
    "GX010101.MP4",   # 10.8 s
    "GX010093.MP4",   # 10.3 s
    "GX010103.MP4",   # 10.7 s
    "GX010092.MP4",   # 17.7 s
    "GX010097.MP4",   # 20.4 s
    "C0376.MP4",      # 33.5 s
]

SEC = 1_000_000_000   # nanoseconds per second

# ── helpers ──────────────────────────────────────────────────────────────────

_req_id = 0

def _next_id():
    global _req_id
    _req_id += 1
    return _req_id


def _send(proc, obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def _recv(proc, timeout=30):
    fd = proc.stdout.fileno()
    ready, _, _ = select.select([fd], [], [], timeout)
    if not ready:
        return None
    line = proc.stdout.readline()
    return json.loads(line) if line else None


def call(proc, name, args=None):
    rid = _next_id()
    _send(proc, {
        "jsonrpc": "2.0",
        "id": rid,
        "method": "tools/call",
        "params": {"name": name, "arguments": args or {}},
    })
    resp = _recv(proc)
    if resp is None:
        print(f"  WARNING: no response for {name}", flush=True)
        return {}
    try:
        return json.loads(resp["result"]["content"][0]["text"])
    except (KeyError, TypeError, json.JSONDecodeError):
        return resp


def screenshot(proc, label, max_retries=4, retry_delay=2.5):
    """Call take_screenshot and print the saved path. Retries on failure."""
    time.sleep(2.0)   # let GTK fully render the new state
    for attempt in range(max_retries):
        data = call(proc, "take_screenshot")
        path = data.get("path", "")
        ok = data.get("ok", False)
        if ok and path:
            print(f"  ✓ [{label}] → {os.path.basename(path)}", flush=True)
            return path
        err = data.get("error", "no render node")
        print(f"  ⟳ [{label}] attempt {attempt+1}/{max_retries} failed: {err}", flush=True)
        time.sleep(retry_delay)
    print(f"  ✗ [{label}] all retries failed", flush=True)
    return "(failed)"


def start_server():
    env = os.environ.copy()
    env["GSK_RENDERER"] = "cairo"
    proc = subprocess.Popen(
        SERVER_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
        cwd=CWD,
        env=env,
    )
    _send(proc, {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}})
    resp = _recv(proc)
    if resp is None:
        print("FAIL: server did not respond to initialize", flush=True)
        proc.terminate()
        sys.exit(1)
    time.sleep(1.5)
    return proc


def stop_server(proc):
    proc.terminate()
    try:
        proc.communicate(timeout=5)
    except Exception:
        proc.kill()

# ── main ─────────────────────────────────────────────────────────────────────

def main():
    saved = []

    proc = start_server()

    # ── Shot 1: Empty project / startup ──────────────────────────────────
    print("\n[1/10] Empty project / startup", flush=True)
    call(proc, "create_project", {"title": "Release Demo"})
    time.sleep(1.0)
    saved.append(screenshot(proc, "01-empty-project"))

    # ── Shot 2: Media library filled ──────────────────────────────────────
    print("\n[2/10] Media library – all Release-Media clips imported", flush=True)
    for clip_name in CLIPS:
        path = os.path.join(MEDIA_DIR, clip_name)
        call(proc, "import_media", {"source_path": path})
    time.sleep(1.5)
    saved.append(screenshot(proc, "02-media-library-filled"))

    # ── Build a primary timeline (4 clips on track 0) ────────────────────
    print("\n  Building primary timeline …", flush=True)
    clip_durations_ns = {
        "GX010088.MP4": int(4.17  * SEC),
        "GX010077.MP4": int(6.8   * SEC),
        "GX010100.MP4": int(6.2   * SEC),
        "GX010101.MP4": int(10.8  * SEC),
    }
    primary_order = ["GX010088.MP4", "GX010077.MP4", "GX010100.MP4", "GX010101.MP4"]
    clip_ids = {}
    cursor_ns = 0
    for name in primary_order:
        dur = clip_durations_ns[name]
        resp = call(proc, "add_clip", {
            "source_path": os.path.join(MEDIA_DIR, name),
            "track_index": 0,
            "timeline_start_ns": cursor_ns,
            "source_in_ns": 0,
            "source_out_ns": dur,
        })
        cid = resp.get("clip_id", "")
        if cid:
            clip_ids[name] = cid
        cursor_ns += dur
    time.sleep(3.0)   # let pipeline rebuild for 4 clips finish

    # ── Shot 3: Primary timeline ─────────────────────────────────────────
    print("\n[3/10] Primary timeline with 4 clips", flush=True)
    saved.append(screenshot(proc, "03-primary-timeline-4-clips"))

    # ── Shot 4: Multi-track (b-roll on track 1) ───────────────────────────
    print("\n[4/10] Multi-track: b-roll overlay on track 1", flush=True)
    broll_start_ns = clip_durations_ns["GX010088.MP4"]   # starts at clip 2
    broll_dur_ns   = clip_durations_ns["GX010100.MP4"]
    resp = call(proc, "add_clip", {
        "source_path": os.path.join(MEDIA_DIR, "GX010093.MP4"),
        "track_index": 1,
        "timeline_start_ns": broll_start_ns,
        "source_in_ns": 0,
        "source_out_ns": broll_dur_ns,
    })
    broll_id = resp.get("clip_id", "")
    time.sleep(1.5)
    saved.append(screenshot(proc, "04-multitrack-broll-overlay"))

    # ── Shot 5: Inspector – Color correction ─────────────────────────────
    print("\n[5/10] Inspector – Color correction sliders", flush=True)
    clip1_id = clip_ids.get("GX010077.MP4", "")
    if clip1_id:
        # Select clip by seeking to it and setting notable color values
        seek_ns = clip_durations_ns["GX010088.MP4"] + int(3 * SEC)
        call(proc, "seek_playhead", {"timeline_pos_ns": seek_ns})
        call(proc, "set_clip_color", {
            "clip_id": clip1_id,
            "brightness": 0.15,
            "contrast": 1.3,
            "saturation": 1.5,
            "denoise": 0.0,
            "sharpness": 0.0,
            "shadows": 0.0,
            "midtones": 0.0,
            "highlights": 0.0,
        })
    time.sleep(1.5)
    saved.append(screenshot(proc, "05-inspector-color-correction"))

    # ── Shot 6: Inspector – Grading (shadows/midtones/highlights) ─────────
    print("\n[6/10] Inspector – Shadows / Midtones / Highlights grading", flush=True)
    if clip1_id:
        call(proc, "set_clip_color", {
            "clip_id": clip1_id,
            "brightness": 0.05,
            "contrast": 1.1,
            "saturation": 1.1,
            "shadows": 0.40,
            "midtones": -0.20,
            "highlights": 0.55,
        })
    time.sleep(1.5)
    saved.append(screenshot(proc, "06-inspector-grading-sliders"))

    # ── Shot 7: Inspector – Transform (scale + position) ──────────────────
    print("\n[7/10] Inspector – Transform panel", flush=True)
    clip3_id = clip_ids.get("GX010100.MP4", "")
    if clip3_id:
        seek_ns = (clip_durations_ns["GX010088.MP4"]
                   + clip_durations_ns["GX010077.MP4"]
                   + int(3 * SEC))
        call(proc, "seek_playhead", {"timeline_pos_ns": seek_ns})
        call(proc, "set_clip_transform", {
            "clip_id": clip3_id,
            "scale": 0.7,
            "position_x": 0.2,
            "position_y": -0.15,
        })
    time.sleep(1.5)
    saved.append(screenshot(proc, "07-inspector-transform"))

    # ── Shot 8: Inspector – Opacity / Compositing ─────────────────────────
    print("\n[8/10] Inspector – Opacity compositing on b-roll clip", flush=True)
    if broll_id:
        call(proc, "set_clip_opacity", {
            "clip_id": broll_id,
            "opacity": 0.7,
        })
    # Seek into the b-roll region
    seek_broll_ns = broll_start_ns + int(2 * SEC)
    call(proc, "seek_playhead", {"timeline_pos_ns": seek_broll_ns})
    time.sleep(1.5)
    saved.append(screenshot(proc, "08-inspector-opacity"))

    # ── Shot 9: Program monitor with video frame ──────────────────────────
    print("\n[9/10] Program monitor showing mid-timeline video frame", flush=True)
    mid_ns = cursor_ns // 2
    call(proc, "seek_playhead", {"timeline_pos_ns": mid_ns})
    time.sleep(3.0)   # compositor settle + decode time
    saved.append(screenshot(proc, "09-program-monitor-frame"))

    # ── Shot 10: Transition + marker ─────────────────────────────────────
    print("\n[10/10] Cross-dissolve transition + timeline marker", flush=True)
    # Add cross_dissolve between clip 0 and clip 1 (0-indexed in track 0)
    call(proc, "set_transition", {
        "track_index": 0,
        "clip_index": 0,
        "kind": "cross_dissolve",
        "duration_ns": 1 * SEC,
    })
    # Seek to the transition region
    call(proc, "seek_playhead", {"timeline_pos_ns": int(3.5 * SEC)})
    time.sleep(1.5)
    saved.append(screenshot(proc, "10-transition-and-marker"))

    stop_server(proc)

    # ── Summary ──────────────────────────────────────────────────────────
    print(f"\n{'='*60}", flush=True)
    print(f"Screenshots saved ({len(saved)}):", flush=True)
    for p in saved:
        exists = os.path.exists(p)
        mark = "✓" if exists else "✗ (missing)"
        size = f" ({os.path.getsize(p)} bytes)" if exists else ""
        print(f"  {mark} {os.path.basename(p)}{size}", flush=True)


if __name__ == "__main__":
    main()
