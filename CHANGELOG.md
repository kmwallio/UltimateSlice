# Changelog

All notable project changes and progress should be recorded here.

## Unreleased

### Added
- Source Monitor — frame-accurate jog/shuttle control:
  - Frame step forward/backward buttons (◀▮ / ▮▶) in source monitor transport bar.
  - Left/Right arrow keyboard shortcuts for single-frame stepping.
  - J/K/L keyboard shortcuts for shuttle reverse/pause/forward at increasing speeds (1×, 2×, 4×).
  - Frame-accurate seeking via new `Player::seek_accurate()` (uses GStreamer `ACCURATE` flag).
  - `Player::step_forward()` / `step_backward()` methods for frame-level navigation.
  - Frame-accurate timecode display (`H:MM:SS:FF`) in position/duration label.
- Source Monitor — dedicated mark-in / mark-out timecode bar:
  - New styled `.marks-bar` showing In, Out, and Duration timecodes with frame accuracy.
  - In-point (green), Out-point (orange), and Duration labels with monospace font.
  - `SourceMarks.frame_ns` field for configurable frame duration (defaults to 24 fps).
- MCP `export_mp4` tool:
  - Added `McpCommand::ExportMp4` and MCP tool schema/dispatch (`export_mp4`).
  - Added main-thread handler in `window.rs` to run export in a background worker and return JSON results.
- Agent workflow rule:
  - Added instruction that new user-facing features should also be added to MCP (when automatable and not already exposed).

### Fixed
- MP4 export audio tracks:
  - Export previously produced silent video due to `-an` flag, `a=0` in concat filter, and audio tracks never being consulted.
  - Fixed: embedded audio from `ClipKind::Video` clips is extracted via `[i:a]adelay=DELAY:all=1` and mixed with `amix`.
  - Audio-only clips from dedicated audio tracks are also included and positioned at their timeline offsets.
  - `ClipKind::Image` clips and video clips without an audio stream (detected via `ffprobe`) are safely skipped.
  - Output is encoded as AAC 192 kbps stereo alongside the existing H.264 video stream.
  - Fixed missing `;` separator between the last `adelay` output label and the `amix` input list in the filter complex string (caused ffmpeg EINVAL / exit 234).
  - MCP export confirmed working end-to-end on `sample-project.fcpxml` (5 clips, ~60s AAC output).
- MP4 export ffmpeg discovery:
  - `Command::new("ffmpeg")` failed when the app's process PATH did not include `/usr/bin`.
  - Added `find_ffmpeg()` which tries the bare name first, then falls back to common absolute paths (`/usr/bin/ffmpeg`, `/usr/local/bin/ffmpeg`, `/opt/homebrew/bin/ffmpeg`).
  - Added `probe_has_audio()` using co-located `ffprobe` to check for audio streams before building the filter graph.
- MP4 export error visibility:
  - FFmpeg error output was silently discarded; exit failures only reported the exit code.
  - Non-progress stderr lines are now captured, logged via `eprintln!`, and included in the returned error message.
- MP4 export reliability:
  - Reworked export pipeline to use ffmpeg clip concat/transcode with per-clip in/out trimming.
  - Normalized sample aspect ratio (`setsar=1`) to prevent concat filter mismatch across mixed sources.
  - Confirmed MCP sample export success:
    - `Sample-Media/mcp-export-test.mp4` (~4.29s)
    - `Sample-Media/mcp-export-full.mp4` (~64.83s)
- Project load visibility:
  - Ensured timeline redraw/content-height update after project load.
  - Reset timeline view state (playhead/scroll/zoom/selection) on New/Open.
  - Synced project clip sources into media library and refreshed browser list when library changes externally.
- Media browser interaction:
  - Fixed click-to-select conflict introduced by drag source handling.
- Timeline scrubber interaction:
  - Fixed continuous click-and-drag scrubbing on the timeline ruler/playhead.
  - Scrubbing now works even when Razor tool is active.
  - Fixed timeline click/seek jumping back to 0 by syncing timeline playhead from program timeline position (not source monitor player position).

### Previous implemented milestones (recent)
- Program monitor playback panel and timeline-linked seeking.
- Audio waveform rendering in timeline audio tracks.
- Drag-and-drop from media browser to timeline.
- Keyboard shortcut overlay and export progress dialog.
- Comprehensive dark theme CSS coverage.
