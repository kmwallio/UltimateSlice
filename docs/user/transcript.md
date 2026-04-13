# Transcript-Based Editing

The Transcript panel lets you edit the timeline by editing the words your
clips are saying. Instead of finding the right cut point on the ruler, you
find the right *word* in the transcript and delete it — UltimateSlice splits
the underlying clip and ripple-deletes the slice for you, in a single undo
entry.

It builds on the existing Whisper-based subtitle pipeline: any clip you have
already run **Generate Subtitles** on shows up in the panel automatically, and
that spoken text is also saved back into the Media Library so library search
and smart collections can find clips by what they say.

---

## Opening the panel

Click **Show Transcript** in the track-management bar at the bottom of the
timeline. (It sits next to **Show Keyframes** — both share the same lower
slot, so opening one closes the other.) Click **Hide Transcript** to collapse
it again.

The panel lists every clip on the current timeline that has subtitles, in
timeline order. Each clip is preceded by its name in square brackets:

```
[clip-name]
The quick brown fox jumps over
the lazy dog.

[next-clip]
And then this happens.
```

If a clip has no transcript yet, it does not appear. Generate one from the
**Inspector → Subtitles → Generate Subtitles** action and the panel will
refresh automatically.

When you are inside a compound clip (double-click a compound on the timeline
to drill in), the Transcript panel scopes itself to the compound's internal
clips just like the timeline does.

---

## Click to seek

Single-click any word and the playhead jumps to the start of that word in
the timeline. The Program Monitor moves with it. While the project plays
back, the currently spoken word is highlighted in yellow so you can follow
along.

The highlight is driven by the same 33 ms tick that updates the Program
Monitor and the keyframe dopesheet, so it stays in sync with playback even
under heavy load.

---

## Selecting a range to delete

Click the first word you want to remove, then **Shift-click** the last word
in the range. The selected words are highlighted in blue.

Selections must stay **within a single clip**. If you Shift-click a word in
a different clip, the panel ignores the click and shows
"Selection must stay within one clip" in the panel's status line. Delete one
clip's range first, then make a fresh selection in the next clip.

---

## Deleting the selection

With a range selected, press **Delete** or **Backspace** (or click the
**Delete Range** button at the top of the panel). UltimateSlice will:

1. Split the underlying clip at the start of the selection.
2. Split it again at the end of the selection.
3. Drop the middle slice.
4. Slide every clip *after* the original clip's right edge **left** by the
   deleted timeline duration, so the gap closes. Intentional gaps elsewhere
   on the track are preserved.

The whole edit is **one undo entry** — press **Ctrl+Z** once to get
everything back. Subtitles on each new half are clamped to its visible
content range and re-based to clip-local time so the karaoke highlight stays
in sync.

Speed-adjusted clips are handled correctly: a 2× clip whose word range
spans 2 seconds of source will lose 1 second of timeline time, and
downstream clips will shift by exactly that amount.

---

## Keyboard shortcuts

| Action | Keys |
|---|---|
| Toggle the Transcript panel | **Show Transcript** button |
| Seek to a word | **Click** a word |
| Extend the selection | **Shift-click** another word in the same clip |
| Delete the selected word range | **Delete** / **Backspace** |

The Transcript panel must have keyboard focus for **Delete** /
**Backspace** to be captured — click anywhere inside the panel first if
keyboard input goes elsewhere.

See [shortcuts.md](shortcuts.md) for the global shortcut reference.

---

## Automation: `delete_transcript_range` MCP tool

The same operation is available through MCP. Word indices reference the
flattened word list of the clip (segment 0 word 0, segment 0 word 1,
segment 1 word 0, ...) which you can read from `list_clips` or
`get_clip_subtitles`.

```json
{
  "name": "delete_transcript_range",
  "arguments": {
    "clip_id": "abc-123",
    "start_word_index": 4,
    "end_word_index": 7
  }
}
```

`end_word_index` is **exclusive** — the example above deletes words at
indices 4, 5, and 6.

The MCP handler calls the same `TimelineState::delete_transcript_word_range`
helper as the keyboard shortcut and the **Delete Range** button, so the
behavior — including undo entry, ripple shifting, and compound-clip
recursion — is identical.

---

## Limits and notes

- v1 only deletes contiguous ranges within a single clip. Multi-clip
  selection and word reordering / cut-and-paste are not supported.
- Audio and video are deleted together (the FCP-style "split + ripple"
  approach). There is no audio-only word removal in v1.
- The **Generate Subtitles** action lives in the Inspector, not the
  Transcript panel — see [inspector.md](inspector.md#subtitles) for
  generation, styling, and SRT export.
- The transcript panel and the keyframe dopesheet share the same bottom
  slot via a `gtk::Stack`. Toggling one closes the other; the most recent
  split position is restored when you reopen either.
