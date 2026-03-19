# Media Library

The **Media Library** panel (left side) holds all source clips available for your project.

## Importing Media

1. When the library is empty, click the **+ Import Media…** button in the library panel.
2. Once media has been imported, use the **+** button next to the **Media Library** title to import more files.
3. Choose one or more video, audio, or image files from the file chooser.
4. Imported items appear in the list showing the clip name and filename.
5. GStreamer probes each file on import to determine its duration and extract source timecode (creation date/time) when available.
6. **Still images** (PNG, JPEG, GIF, BMP, TIFF, WebP, HEIC) are detected by file extension and assigned a **4-second default duration**. They are classified as image clips rather than video or audio.
7. If a source path is unavailable on disk, the media card shows an **OFFLINE** badge and warning outline.

You can also drag files directly from your file manager into the **Media Library** pane to import them.

Supported formats depend on your installed GStreamer plugins (any format `playbin` can decode). Still images are supported natively.

## Browsing and Selecting

- Click a library item to select it — the **Source Monitor** immediately loads and previews the clip.
- The clip name is shown above the source monitor preview.

## Adding Clips to the Timeline

### Append to Timeline

1. Select a clip in the library.
2. Set In/Out points in the Source Monitor if needed (see [source-monitor.md](source-monitor.md)).
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
- Creating a new project or opening a different project clears the current library view first, then loads that project's media list.
- Thumbnails are generated asynchronously and refresh automatically as they become available (no manual panel/window resize needed).
- Source timecode (from camera creation timestamps) is automatically extracted during import and used for timecode-based alignment of grouped clips without manual entry.
- When the library is empty, the panel shows a short hint reminding you that you can import or drag files to begin.

## Relinking offline media

- Use **Relink…** in the main toolbar to recover missing source files.
- Choose a folder to scan. UltimateSlice searches recursively and remaps missing paths by filename, then breaks ties using deepest tail-path match.
- The relink pass reports how many items were remapped and how many remain unresolved.
