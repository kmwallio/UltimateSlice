import subprocess
import json
import time
import os
import sys

# Change to project root if running from elsewhere (not needed if run from root)
SERVER_PATH = "target/debug/ultimate-slice"

def send(proc, request):
    req_json = json.dumps(request)
    print(f"Sending request: {request['method']}")
    proc.stdin.write(req_json + "\n")
    proc.stdin.flush()

def read_response(proc):
    line = proc.stdout.readline()
    if not line:
        return None
    return json.loads(line)

def main():
    if os.path.exists("mcp_test_out.mp4"):
        os.remove("mcp_test_out.mp4")

    print(f"Starting server: {SERVER_PATH}")
    
    # We need to capture stderr to debug ffmpeg issues
    proc = subprocess.Popen(
        [SERVER_PATH, "--mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1, # Line buffered
        universal_newlines=True
    )

    # Initialize
    send(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    resp = read_response(proc)
    print("Initialize:", resp)

    # Helper function to call tool
    def call_tool(id, name, args):
        send(proc, {
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": args}
        })
        return read_response(proc)

    # Add Clip 1 (Video)
    # Duration 5s. Add at 0.
    cwd = os.getcwd()
    file1 = os.path.join(cwd, "Sample-Media/GX010426.MP4")
    file2 = os.path.join(cwd, "Sample-Media/GX010429.MP4")

    resp = call_tool(2, "add_clip", {
        "source_path": file1,
        "track_index": 0,
        "timeline_start_ns": 0,
        "source_in_ns": 0,
        "source_out_ns": 5000000000 # 5s
    })
    print("Add Clip 1:", resp)
    
    if "error" in str(resp):
        print("Failed to add clip 1")
        sys.exit(1)

    # Add Clip 2 (Video)
    # Add at 5s. Duration 5s.
    resp = call_tool(3, "add_clip", {
        "source_path": file2,
        "track_index": 0,
        "timeline_start_ns": 5000000000,
        "source_in_ns": 0,
        "source_out_ns": 5000000000 # 5s
    })
    print("Add Clip 2:", resp)
    
    if "error" in str(resp):
        print("Failed to add clip 2")
        sys.exit(1)

    # Set Transition (Clip 1 -> Clip 2)
    # Duration 1s (1_000_000_000 ns).
    resp = call_tool(4, "set_transition", {
        "track_index": 0,
        "clip_index": 0,
        "kind": "cross_dissolve",
        "duration_ns": 1000000000
    })
    print("Set Transition:", resp)
    
    if "error" in str(resp):
        print("Failed to set transition")
        sys.exit(1)

    # Export
    output_path = os.path.join(cwd, "mcp_test_out.mp4")
    print(f"Exporting to {output_path}...")
    resp = call_tool(5, "export_mp4", {
        "path": output_path
    })
    print("Export Response:", resp)
    
    # Check result
    if "error" in str(resp) or (resp.get("result") and resp["result"]["content"][0]["text"].find("success\":false") != -1):
        print("Export failed!")
    else:
        print("Export success!")
    
    # Cleanup
    proc.terminate()
    try:
        outs, errs = proc.communicate(timeout=5)
        if errs:
            print("STDERR Output:")
            print(errs)
    except:
        proc.kill()
        outs, errs = proc.communicate()
        if errs:
            print("STDERR Output:")
            print(errs)

if __name__ == "__main__":
    main()
