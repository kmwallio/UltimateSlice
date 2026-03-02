---
layout: doc
title: Media Library
permalink: /docs/media-library/
---

# Media Library

The **Media Library** panel (left side) holds all source clips available for your project.

## Importing Media

- **Library Empty**: A large **+ Import Media…** button is shown in the center of the library panel.
- **Library Populated**: The large button is hidden to save space. Click the compact **+** button in the **Media Library** header to add more files.
- **Drag and Drop**: You can also drag files directly from your system's file manager into the **Media Library** pane to import them.

Choose one or more video, audio, or image files from the file chooser. Supported formats depend on your installed GStreamer plugins (any format `playbin` can decode).

GStreamer probes each file on import to determine its duration and type (audio-only or video). This runs in the background to keep the interface responsive.

## Browsing and Selecting

- Click a library item to select it — the **Source Monitor** immediately loads and previews the clip.
- The clip name is shown above the source monitor preview.

## Adding Clips to the Timeline

### Append to Timeline

1. Select a clip in the library.
2. Set In/Out points in the Source Monitor if needed (see [Source Monitor](source-monitor)).
3. Click **Append to Timeline** — the marked region is placed at the end of the first Video track.
   - For audio-only sources, append targets an audio track; for video sources, a video track.
   - If an active matching-kind track is selected in timeline, append uses that track; otherwise it uses the first matching-kind track.

### Drag and Drop

- Drag a library item directly onto a specific track and position in the timeline.
- The clip is placed at the drop position on the target track.

## Notes

- Importing a clip does **not** automatically add it to the timeline.
- Deleting a clip from the timeline does not remove it from the library.
- The library list is saved as part of the FCPXML project file.
