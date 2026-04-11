# Auditions / Clip Versions

Auditions let a single timeline slot hold multiple alternate takes of the same beat. You can swap takes nondestructively, compare them in context, and finalize once the cut locks — without disturbing the surrounding edit, transitions, color grade, or transforms attached to the slot.

If you've used Final Cut Pro's "auditions," this is the same idea: one clip on the timeline, several candidate takes inside it, one designated active take that drives playback and export.

---

## Quick Start

1. Drop two or more candidate clips on the **same track** (a wide, a medium, a close-up; or three reads of a VO line).
2. Select all of them (Ctrl+click or Shift+click).
3. Right-click → **Create Audition from Selection**.
4. The selection collapses to one gold "AUD" clip on the timeline.
5. Select the audition clip. The **Inspector → Audition** panel lists every take.
6. Click any take row to make it active. The Program Monitor and the timeline update immediately.
7. When you're happy, right-click the clip → **Finalize Audition** (or use the Inspector button) to collapse the audition to a normal clip referencing only the active take.

Every step is undoable with `Ctrl+Z`.

---

## What an audition clip is

An audition clip wraps a list of alternate takes around a single timeline slot. The slot's host fields — `source_path`, `source_in`, `source_out`, source timecode, and probed media duration — always mirror whichever take is currently active. Anything you'd normally attach to a clip (color grade, transforms, transitions, masks, frei0r effects, keyframes, position/scale, even motion tracking) is attached to the **slot**, not to the take. So when you switch takes:

- The Program Monitor and export immediately use the new take.
- The clip resizes on the timeline if the new take has a different duration.
- Your color grade, transforms, transitions, and effects on that slot stay put.
- Any field tweaks you made (a tiny trim, a new in-point) while the previous take was active are snapshotted into the takes list before the swap, so switching back recovers them exactly.

Takes can be heterogeneous: different source files, different in/out, different durations, even from different cameras. They just need to live in the same audition clip.

---

## Creating an audition

You need at least two candidate clips on the **same track** of the same kind (all video, all images, or all audio). Mixed-kind selections are refused.

### From the timeline

1. Multi-select 2+ clips on one track.
2. Right-click → **Create Audition from Selection**.

The earliest selected clip's `timeline_start` anchors the audition; the first clip becomes the active take. The other clips are removed from the timeline and stored as inactive takes inside the new audition.

### From MCP automation

```jsonrpc
{"method":"tools/call","params":{"name":"create_audition_clip",
 "arguments":{"clip_ids":["abc-123","def-456","ghi-789"],"active_index":0}}}
```

The response includes the new audition clip's id:

```json
{"success": true, "audition_clip_id": "..."}
```

---

## Switching takes

Select the audition clip on the timeline. The **Inspector → Audition** panel shows a take list. Each row shows the take's label, source filename, and duration. The active row is highlighted with an **Active** badge.

- **Click any other row** to make it active. Undoable.
- The Program Monitor and timeline update right away — no rebuild, no flicker.
- The "n / m" indicator on the clip badge updates ("2 / 3" means take 2 of 3 is active).

### From MCP

```jsonrpc
{"method":"tools/call","params":{"name":"set_active_audition_take",
 "arguments":{"audition_clip_id":"...","take_index":1}}}
```

---

## Adding more takes later

You can keep adding alternates to an existing audition.

### From the Inspector

Click **Add Take from Source** in the Audition section. UltimateSlice creates a new take from the audition's currently active take as a starting point — useful when you want a slightly different in/out on the same source. Switch the active take afterwards if you want to actually compare them.

### From MCP

```jsonrpc
{"method":"tools/call","params":{"name":"add_audition_take",
 "arguments":{"audition_clip_id":"...",
              "source_path":"/footage/take_5.mov",
              "source_in_ns":2000000000,
              "source_out_ns":7000000000,
              "label":"Take 5 — closer crop"}}}
```

---

## Removing a take

Select a non-active row in the Inspector takes list and click **Remove Take**. The button is greyed out for the active take — switch the active take first if you want to delete what's currently playing.

### From MCP

```jsonrpc
{"method":"tools/call","params":{"name":"remove_audition_take",
 "arguments":{"audition_clip_id":"...","take_index":2}}}
```

The active take is protected — the call returns an error if you try to remove it.

---

## Finalizing an audition

When the cut is locked and you don't need the alternates anymore, finalize the audition. This collapses it to a normal clip referencing only the active take and discards every other take. The audition badge and the takes list go away; everything else (color, transforms, effects, transitions) stays in place.

- **From the Inspector** — click the destructive **Finalize Audition** button.
- **From the timeline** — right-click the audition clip → **Finalize Audition**.
- **From MCP** — `finalize_audition` (see below).

The clip kind is auto-detected from the active take's file extension (video → `Video`, audio → `Audio`, image extensions → `Image`).

Finalization is undoable with `Ctrl+Z` — undo restores the audition wrapper and every alternate take.

### From MCP

```jsonrpc
{"method":"tools/call","params":{"name":"finalize_audition",
 "arguments":{"audition_clip_id":"..."}}}
```

---

## Listing takes (read-only)

For tooling and scripts:

```jsonrpc
{"method":"tools/call","params":{"name":"list_audition_takes",
 "arguments":{"audition_clip_id":"..."}}}
```

Response shape:

```json
{
  "clip_id": "...",
  "active_take_index": 1,
  "takes": [
    {
      "index": 0,
      "id": "take-...",
      "label": "Wide",
      "source_path": "/footage/wide.mov",
      "source_in_ns": 0,
      "source_out_ns": 5000000000,
      "source_timecode_base_ns": null,
      "media_duration_ns": 10000000000
    },
    {
      "index": 1,
      "id": "take-...",
      "label": "Close",
      "source_path": "/footage/close.mov",
      "source_in_ns": 1000000000,
      "source_out_ns": 4000000000,
      "source_timecode_base_ns": null,
      "media_duration_ns": 8000000000
    }
  ]
}
```

---

## How auditions are saved

### `.uspxml` (UltimateSlice native)

Audition clips persist losslessly with all alternate takes:

- `us:clip-kind="audition"`
- `us:audition-takes="..."` — JSON-encoded array of takes
- `us:audition-active-take-index="0"`

Reopening the project gives you the audition exactly as you left it, with the same active take and the same alternates available for further A/B comparison.

### `.fcpxml` (Final Cut Pro interchange — strict mode)

Strict-mode FCPXML export does **not** carry the alternate takes. The audition collapses to a plain `<asset-clip>` referencing the **currently active take**, identical to what `Finalize Audition` would have produced. This keeps the export DTD-clean and round-trips correctly to Final Cut Pro, but the alternates are not in the file. If you want to ship your alternates between machines or back to a future editing session, save as `.uspxml` instead.

### `.otio` (OpenTimelineIO)

OTIO has no native audition concept, so UltimateSlice stores audition data as vendor metadata under `metadata.ultimateslice.audition_takes` and `metadata.ultimateslice.audition_active_take_index`. Reimport restores the full audition (takes, active index, host fields).

---

## Tips and gotchas

- **Same track only.** Auditions live in one timeline slot, so all source clips must be on one track. Mixed-track selections refuse to create an audition. If you want to A/B between candidates on different tracks, move them to the same track first.
- **Same kind only.** All takes in one audition must be the same kind — all video clips, all image stills, or all audio clips. Mixing kinds isn't supported.
- **No automatic ripple on take swap.** Different takes can have different durations. When you switch to a longer take, the clip resizes in place — it does not push subsequent clips down the timeline. If a longer take overlaps a neighbor, resolve it with normal trimming.
- **Color grade and effects stay on the slot.** Switching takes preserves color, transforms, transitions, masks, keyframes, and frei0r effects. This is the whole point — you can dial in your look once and try every alternate against it.
- **Active take is protected from deletion.** Switch active first if you want to remove the take you're currently looking at.
- **Undo restores everything.** Every audition operation (create, add, remove, switch active, finalize) goes through the undo system, so `Ctrl+Z` walks back through every change just like any other edit.

---

## See also

- [timeline.md](timeline.md) — multi-select, right-click context menu, related editing tools
- [inspector.md](inspector.md) — Inspector panel layout
- [python-mcp.md](python-mcp.md) — MCP automation for auditions and other features
