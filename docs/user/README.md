# UltimateSlice — User Documentation

Welcome to UltimateSlice, a Final Cut Pro–inspired non-linear video editor built with GTK4 and Rust.

---

## Feature Guides

| Document | What it covers |
|---|---|
| [getting-started.md](getting-started.md) | Installation, first launch, creating your first project |
| [media-library.md](media-library.md) | Importing media, browsing clips, appending to timeline |
| [source-monitor.md](source-monitor.md) | Previewing clips, setting In/Out points, shuttle controls |
| [timeline.md](timeline.md) | Arranging clips, trimming, splitting, markers, zoom/pan |
| [VIDEOAUDIOALIGNMENT.md](VIDEOAUDIOALIGNMENT.md) | Multi-cam sync: timecode alignment and audio cross-correlation |
| [inspector.md](inspector.md) | Color correction, effects, audio, transform, titles, speed |
| [effects.md](effects.md) | Complete effects reference: color grading, frei0r plugins, blend modes, chroma key, LUTs |
| [color-scopes.md](color-scopes.md) | Waveform, histogram, RGB parade, vectorscope |
| [preferences.md](preferences.md) | Application-level settings and performance preferences |
| [workspace-layouts.md](workspace-layouts.md) | Saving and restoring panel arrangements for different tasks |
| [python-mcp.md](python-mcp.md) | Python socket client commands for MCP |
| [program-monitor.md](program-monitor.md) | Previewing the assembled timeline |
| [export.md](export.md) | Advanced export and interchange: codecs, resolution, audio options, OTIO/EDL |
| [project-settings.md](project-settings.md) | Canvas size, frame rate, FCPXML save/load, snapshots, backups |
| [shortcuts.md](shortcuts.md) | Complete keyboard shortcut reference |

For playback tuning and render-cache behavior, start with [preferences.md](preferences.md) and [program-monitor.md](program-monitor.md); for saved panel arrangements, see [workspace-layouts.md](workspace-layouts.md); MCP automation examples for the same controls live in [python-mcp.md](python-mcp.md).

---

## Quick-Start Summary

1. **Import media** — click **Import Media** in the Media Library panel.
2. **Mark and triage your clip** — select it in the library; use **I** / **O** to set In/Out points in the Source Monitor, optionally add a keyword range, and use Favorite/Reject browser ratings while sorting selects.
3. **Append to timeline** — click **Append to Timeline** or drag the clip from the library onto the timeline.
4. **Arrange** — drag clips to reposition; drag their edges to trim; press **B** for the Razor tool to split.
5. **Adjust** — select a clip and use the Inspector panel (right side) for color, audio, speed, and titles.
6. **Export** — click **Export…** in the toolbar and choose your codec, resolution, and output file.

For AI-generated music beds, right-click an audio track header and choose **Generate Music Region…**, then drag an empty 1-30 second range and enter a prompt.

For dialogue tone matching, select a timeline clip and use **Inspector → Audio → Match Audio…** with a reference clip. The dialog now defaults to a simple **Match voice** mode, while **Choose region...** unlocks exact source/reference timecode ranges for power users. The same workflow is available through MCP `match_clip_audio`.
