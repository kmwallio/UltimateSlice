# Project Settings

Click **⚙ Settings** in the toolbar to open the Project Settings dialog.

## Aspect Ratio

Choose an aspect ratio to filter available resolution presets:

| Option | Description |
|---|---|
| **16:9 (Widescreen)** | Standard widescreen — TV, YouTube, cinema |
| **4:3 (Standard)** | Classic TV / presentation format |
| **9:16 (Vertical)** | Portrait — mobile stories, reels, TikTok |
| **1:1 (Square)** | Square — Instagram, social media |
| **Custom** | Enter any width × height manually |

## Resolution Presets

When a preset aspect ratio is selected, the Resolution dropdown shows matching canvas sizes:

### 16:9 (Widescreen)

| Preset | Resolution |
|---|---|
| 3840 × 2160 (4K UHD) | 3840 × 2160 |
| 2560 × 1440 (1440p QHD) | 2560 × 1440 |
| 1920 × 1080 (1080p HD) | 1920 × 1080 |
| 1280 × 720 (720p HD) | 1280 × 720 |

### 4:3 (Standard)

| Preset | Resolution |
|---|---|
| 1440 × 1080 (HD 4:3) | 1440 × 1080 |
| 1024 × 768 (XGA) | 1024 × 768 |
| 720 × 480 (SD NTSC) | 720 × 480 |

### 9:16 (Vertical)

| Preset | Resolution |
|---|---|
| 1080 × 1920 (Full HD Vertical) | 1080 × 1920 |
| 720 × 1280 (HD Vertical) | 720 × 1280 |

### 1:1 (Square)

| Preset | Resolution |
|---|---|
| 2160 × 2160 (4K Square) | 2160 × 2160 |
| 1080 × 1080 (HD Square) | 1080 × 1080 |

## Custom Resolution

Select **Custom** from the Aspect Ratio dropdown to enter any width and height using spin buttons:

- **Width**: 128–7680 pixels (step 2)
- **Height**: 128–4320 pixels (step 2)

All clips are automatically scaled and letterboxed/pillarboxed to fit the chosen canvas during both preview and export.

## Frame Rate

| Preset | Rate | Common Use |
|---|---|---|
| **23.976 fps** | 24000/1001 | Film / cinema (NTSC pulldown) |
| **24 fps** | 24/1 | True cinema |
| **25 fps** | 25/1 | PAL broadcast / European TV |
| **29.97 fps** | 30000/1001 | NTSC broadcast / US TV |
| **30 fps** | 30/1 | Web video |
| **60 fps** | 60/1 | High frame rate / gaming |

## Applying Changes

- Click **Apply** to confirm. Changes take effect immediately.
- The project is marked dirty (unsaved) after applying.

## Saving

Project settings (resolution and frame rate) are saved as the `<format>` element in the FCPXML file. UltimateSlice always writes numeric format fields (`width`, `height`, `frameDuration`) and only writes a canonical `name` when the preset is known, which improves interoperability with other editors.
Source media references are saved as nested `<media-rep>` entries under each resource `<asset>`; non-proxy files are tagged `kind="original-media"`. Exported `file://` media paths are URI-safe (percent-encoding spaces and other URI-unsafe characters).

Use **Save…** (`Ctrl+S`) to write the project as XML (default suggested filename: `project.uspxml`; `.fcpxml` remains fully supported). Open with **Open…** (`Ctrl+O`) on any future session.

Save format behavior is extension-based:
- Saving as `.uspxml` keeps UltimateSlice feature-rich round-trip metadata (including vendor `us:*` fields and unknown passthrough fields where applicable).
- Saving as `.fcpxml` uses strict compatibility output (no UltimateSlice vendor namespace/attrs, with strict DTD-friendly structure) for broader interoperability. Strict output includes standards-native `lane` mapping for multi-track timeline layering and enforces DTD-ordered intrinsic params in clips.

Use **Export ▼ → Export Project with Media…** to create a portable package:
- Writes the chosen `.uspxml`/`.fcpxml` file.
- Copies all timeline-used source media into a sibling `ProjectName.Library` folder (based on the output filename stem).
- Rewrites saved media references to point at those copied library files.
- Writes packaged XML in a strict compatibility mode that omits UltimateSlice vendor `us:*` attributes and passthrough unknown XML fields for better strict FCPXML 1.14 validator interoperability.
- For packaged exports targeting external Linux mount roots (`/media`, `/run/media`, `/mnt`), paths are normalized to `/Volumes/<drive>/...` in the XML for better macOS and cross-distro portability.
- If different source files share the same filename, UltimateSlice keeps the first name and adds deterministic suffixes for collisions.
- Shows an export progress window while copying media and writing the packaged project XML.

Use **Export ▼ → Collect Files…** when you want the media copy without saving project XML:
- **Timeline-used only** copies just the source files referenced by clips on the timeline.
- **Entire library** also copies imported media that is currently unused on the timeline.
- Clip LUT files that exist on disk are copied too.
- Files are copied into the folder you choose; if names collide, UltimateSlice keeps the first name and adds deterministic suffixes for the rest.
- Optional **Use collected locations on next save** updates the open project to point at the copied media/LUT files after collection finishes, so the next project save/export writes the collected paths instead of the old locations.
- No project XML is written or rewritten by this workflow.

When you open an existing FCPXML and save it without making edits, UltimateSlice preserves the original document verbatim so unknown attributes/fields from other tools are retained.
For edited saves, unsupported `asset-clip` attributes/child tags and imported resource `<asset>` metadata payloads (including nested `<metadata><md .../></metadata>`) are still carried forward in regenerated output. Unknown attrs/child tags are also preserved across core document structure (`<fcpxml>`, `<resources>`, selected `<library>/<event>/<project>/<sequence>/<spine>`, plus selected sequence `<format>` attrs) during dirty-save regeneration. Regenerated documents include `<!DOCTYPE fcpxml>` and keep source references in nested `<media-rep>` entries rather than legacy `asset@src`.
UltimateSlice also reads and writes native spine `<transition>` elements, mapping them to clip transition settings so common transition timing/name metadata interoperates better with other FCPXML tools.
UltimateSlice also reads and writes native `<timeMap>/<timept>` for constant 2-point retimes (speed changes, reverse playback, and full-clip freeze holds) plus representable multi-point monotonic ramps mapped to speed keyframes. `timept@interp` smooth modes (`smooth2`/`smooth`) are mapped to eased keyframe interpolation for representable ramps, while native maps that depend on `inTime`/`outTime` handles (or other unsupported semantics) are preserved and re-emitted in the proper timing-params position instead of being lossy-remapped.
When possible for imported projects, transform-only dirty saves patch matching `adjust-transform` nodes in-place on the original XML (including nested clips), which keeps the saved file much closer to the source structure and IDs.
For transform position compatibility, UltimateSlice converts FCPXML `adjust-transform@position` values using frame-height percentage semantics (both X and Y percentages based on frame height, center-origin) to and from UltimateSlice's internal scale-aware position model (with Y-axis inversion) during import/export.
For broader import compatibility, spine `ref-clip` elements are mapped through their referenced assets, and `sync-clip` wrappers (including nested `spine` containers) are traversed so nested `asset-clip`/`ref-clip` story elements are imported instead of ignored as opaque unknown blocks.
For FCPXML assets that use absolute source-time domains, UltimateSlice rebases `asset-clip@start` against `asset@start` during import so layered lane clips seek/play from the intended media-relative source position.
When imported FCPXML media references start with `/Volumes/...` and are missing locally, UltimateSlice URI-decodes the path (for example `%20` → space), retries common Linux external-drive mount locations (`/media/<user>/...`, `/run/media/<user>/...`, `/media/...`, `/run/media/...`, `/mnt/...`) plus the opened FCPXML mount-root fallback, then uses the found file for runtime playback while still saving the original imported XML source path.

## Auto-Save

UltimateSlice auto-saves every 60 seconds to `/tmp/ultimateslice-autosave.fcpxml` when the project has unsaved changes. This is a safety net — use **Save…** for permanent storage.

## Named Snapshots

Use **Export ▼ → Create Snapshot…** to save the current project as a named milestone such as `Before color pass` or `Client v2`.

- Use **Export ▼ → Manage Snapshots…** to browse snapshots for the current project, then restore or delete them.
- Snapshots are stored in `~/.local/share/ultimateslice/snapshots/` (or `$XDG_DATA_HOME/ultimateslice/snapshots/` if set) as managed `.uspxml` versions plus snapshot metadata.
- Restoring a snapshot loads that version into the editor and keeps the current primary save target unchanged; the restored project is marked dirty until you save again.
- MCP tools: `list_project_snapshots`, `create_project_snapshot`, `restore_project_snapshot`, and `delete_project_snapshot`

## Versioned Backups

In addition to auto-save, UltimateSlice creates **timestamped backup copies** of your project every 60 seconds (when dirty) in:

```
~/.local/share/ultimateslice/backups/
```

(or `$XDG_DATA_HOME/ultimateslice/backups/` if set)

- Backups are named `{ProjectTitle}_{YYYYMMDD_HHMMSS}.uspxml`
- Old backups are automatically pruned to keep the most recent N versions per project title (default 20)
- **Restore from Backup**: Use the **Export ▼ → Restore from Backup…** menu item to browse and load a previous backup without silently retargeting the project's main save path
- Configure in **Preferences → General**: toggle "Auto-backup" on/off and set "Max backup versions"
- MCP tool: `list_backups` lists available backup files with sizes
