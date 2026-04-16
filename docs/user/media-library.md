# Media Library

The **Media Library** panel (left side) holds all imported source clips available for your project, along with timeline-native browser items such as titles that do not have backing media files.

## Importing Media

1. When the library is empty, use the centered **Import Media…** action in the library panel.
2. Once media has been imported, use the **+** button next to the **Media Library** title to import more files.
3. Choose one or more video, audio, or image files from the file chooser.
4. Imported items appear as thumbnail cards showing the clip name and, once probing completes, compact media metadata.
5. GStreamer probes each file on import to determine duration, media type, dimensions/frame rate when applicable, file size, and source timecode (creation date/time) when available.
6. **Still images** (PNG, JPEG, GIF, BMP, TIFF, WebP, HEIC, static SVG) are detected by file extension and assigned a **4-second default duration**. They are classified as image clips rather than video or audio.
7. **Animated SVG** sources are detected during import, keep their authored animation duration, and render to a cached silent video for preview, thumbnails, timeline playback, and export. The current implementation supports the SMIL-style `<animate>` / `<animateTransform>` subset; JavaScript and broader browser-style CSS animation behavior are not supported.
8. If a source path is unavailable on disk, the media card shows an **OFFLINE** badge and warning outline.

You can also drag files directly from your file manager into the **Media Library** pane to import them.

While the library is empty, the panel keeps that import action centered so the first step stays obvious on a fresh project.

Supported formats depend on your installed GStreamer plugins (any format `playbin` can decode). Still images are supported natively.

## Bins / Folders

Organize your media into **bins** (folders) for large projects.

### Creating Bins

- **Right-click** on empty space in the library and choose **New Bin…**
- Enter a name and press **Create**
- Bins can be nested up to 2 levels deep (right-click a bin → **New Sub-bin…**)

### Navigating Bins

- **Double-click** a bin folder to enter it
- Use the **breadcrumb bar** above the grid to navigate back to parent bins or root
- Click the **All** button in the header to see all media regardless of bin

### Managing Items in Bins

- **Drag** a media item onto a bin folder to move it into that bin
- **Right-click** a media item → **Move to "Bin Name"** to move it to a bin
- **Right-click** a media item → **Move to Root** to move it back to the top level
- Items imported while viewing a bin are automatically placed in that bin
- **Right-click** a bin → **Rename…** or **Delete** (items move to the parent or root)

Bins are saved with your project and restored when you reopen it.

## Browsing and Selecting

- Click a **source-backed** library item to select it — the **Source Monitor** immediately loads and previews the clip.
- The clip name is shown above the source monitor preview.
- Title and other non-file-backed browser cards remain visible/searchable and can still be organized into bins, but they do not load the Source Monitor because they have no source file to preview.

## Metadata and Filtering

- Each media card now shows compact metadata beneath the clip name when available:
  - **Video / animated SVG**: resolution, frame rate, codec summary, duration, file size
  - **Audio-only**: audio-only indicator text, codec summary, duration, file size
  - **Still images**: resolution, image type, default duration, file size
- Timeline-native cards with no backing file show their clip type instead of file metadata. Title cards use the current title text as the main card label and remain searchable by that text.
- Favorite/reject ratings appear directly on media cards, and keyword ranges show a compact summary line when the clip has saved ranges.
- Hover a media card to see the full source path plus expanded metadata details in the tooltip, including rating and individual keyword ranges when present.
- Use the **filter search** field to match clip names, title text, file paths, codec text, keyword labels, or stored spoken transcript text from subtitle-generation workflows.
- When the current query matches spoken content, matching clips show a short **Spoken:** hint on the card and the tooltip includes the matched transcript excerpt plus the clip's transcript-segment count.
- If **Preferences → Models → AI index in background** is enabled, eligible audio-backed library items with no transcript cache are queued automatically after import/open so older projects can backfill spoken-content search without manually regenerating subtitles for every clip. The preference is disabled by default.
- Use the **type** dropdown to focus on video, audio, images, or offline clips.
- Use the **size** dropdown to narrow the current browser scope to SD-or-smaller, HD, Full HD, or 4K+ media.
- Use the **FPS** dropdown to narrow the current browser scope to 24 fps-or-less, 25-30 fps, 31-59 fps, or 60+ fps clips.
- Use the **Ratings** dropdown to narrow the current browser scope to Favorite, Reject, or Unrated clips.
- Filters apply to the current browser scope:
  - inside a bin, they filter that bin's items
  - in **All Media**, they filter the flat project-wide media view
  - bins themselves remain visible so navigation still works while filters are active

## Ratings and Keyword Ranges

- **Right-click** one or more selected media items to **Mark Favorite**, **Mark Reject**, or **Clear Rating** in one step.
- Ratings are editorial triage state only; they do not affect timeline playback or export.
- Keyword ranges are authored from the **Source Monitor** using the current In/Out marks on the selected source clip.
- Keyword summaries stay attached to the source media item, so the same ranges remain available anywhere that media appears in the project.

## Smart Collections

- Use the **Collections** picker in the filter bar to recall saved project-wide media queries.
- Click the **save** button next to the picker to store the current search/type/size/FPS/rating filter combination as a smart collection, including transcript-aware search text.
- Selecting a smart collection switches the browser to a flat **All Media**-style view across the whole project, even if you were previously inside a bin.
- Use the **rename** and **delete** buttons next to the picker to manage the selected collection.
- Smart collections are saved with your project, round-trip through UltimateSlice's FCPXML vendor metadata, and are available to automation through the MCP `list_collections`, `create_collection`, `update_collection`, and `delete_collection` tools.
- MCP `list_library` now includes each item's stable `library_key`, rating, keyword ranges, transcript metadata, and optional `search_match` details when called with `search_text`; browser annotations can be automated with `set_media_rating`, `add_media_keyword_range`, `update_media_keyword_range`, and `delete_media_keyword_range`.

## Adding Clips to the Timeline

### Append to Timeline

1. Select a clip in the library.
2. Set In/Out points in the Source Monitor if needed (see [source-monitor.md](source-monitor.md)).
3. Click **Append to Timeline** — the marked region is placed at the end of a matching timeline track.
   - For audio-only sources, append targets an audio track; for video or image sources, a video track.
   - If an active matching-kind track is selected in timeline, append uses that track; otherwise it uses the first matching-kind track.
   - If no matching track exists yet, UltimateSlice creates one automatically before placing the clip.

### Drag and Drop

- Drag a library item directly onto a specific track and position in the timeline.
- The clip is placed at the drop position on the target track.
- You can also drag files from your file manager directly onto the **timeline** — they are automatically imported into the media library and placed as clips at the drop position. Multiple files are placed sequentially.

## Notes

- Importing a clip does **not** automatically add it to the timeline.
- Deleting a clip from the timeline does not remove it from the library.
- The library list is saved as part of the FCPXML project file.
- Creating a new project or opening a different project clears the current library view first, then loads that project's media list.
- Thumbnails are generated asynchronously and refresh automatically as they become available (no manual panel/window resize needed).
- Source timecode (from camera creation timestamps) is automatically extracted during import and used for timecode-based alignment of grouped clips without manual entry.
- When the library is empty, the panel switches to a centered first-run state with the main **Import Media…** action and a reminder that you can also drag files in directly.

## Relinking offline media

- Use **Relink…** in the main toolbar to recover missing source files.
- Choose a folder to scan. UltimateSlice searches recursively and remaps missing paths by filename, then breaks ties using deepest tail-path match.
- The relink pass reports how many items were remapped and how many remain unresolved.
