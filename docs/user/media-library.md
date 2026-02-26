# Media Library

The **Media Library** panel (left side) holds all source clips available for your project.

## Importing Media

1. Click **Import Media** at the bottom of the library panel.
2. Choose one or more video, audio, or image files from the file chooser.
3. Imported items appear in the list showing the clip name and filename.
4. GStreamer probes each file on import to determine its duration.

Supported formats depend on your installed GStreamer plugins (any format `playbin` can decode).

## Browsing and Selecting

- Click a library item to select it — the **Source Monitor** immediately loads and previews the clip.
- The clip name is shown above the source monitor preview.

## Adding Clips to the Timeline

### Append to Timeline

1. Select a clip in the library.
2. Set In/Out points in the Source Monitor if needed (see [source-monitor.md](source-monitor.md)).
3. Click **Append to Timeline** — the marked region is placed at the end of the first Video track.

### Drag and Drop

- Drag a library item directly onto a specific track and position in the timeline.
- The clip is placed at the drop position on the target track.

## Notes

- Importing a clip does **not** automatically add it to the timeline.
- Deleting a clip from the timeline does not remove it from the library.
- The library list is saved as part of the FCPXML project file.
