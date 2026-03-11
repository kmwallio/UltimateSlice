# Project Settings

Click **⚙ Settings** in the toolbar to open the Project Settings dialog.

## Canvas Size (Resolution)

Choose the output frame size for the project:

| Preset | Resolution | Aspect Ratio | Use Case |
|---|---|---|---|
| **1920 × 1080 (1080p HD)** | 1920 × 1080 | 16:9 | Standard HD (default) |
| **3840 × 2160 (4K UHD)** | 3840 × 2160 | 16:9 | 4K delivery |
| **1280 × 720 (720p HD)** | 1280 × 720 | 16:9 | Web / streaming |
| **720 × 480 (SD NTSC)** | 720 × 480 | ~4:3 | Standard definition |
| **1080 × 1920 (9:16 Vertical)** | 1080 × 1920 | 9:16 | Mobile / stories / reels |
| **1080 × 1080 (1:1 Square)** | 1080 × 1080 | 1:1 | Social media square |

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
Source media references are saved as nested `<media-rep>` entries under each resource `<asset>`; non-proxy files are tagged `kind="original-media"`.

Use **Save…** (`Ctrl+S`) to write the project as XML (default suggested filename: `project.uspxml`; `.fcpxml` remains fully supported). Open with **Open…** (`Ctrl+O`) on any future session.

Use **Export ▼ → Export Project with Media…** to create a portable package:
- Writes the chosen `.uspxml`/`.fcpxml` file.
- Copies all timeline-used source media into a sibling `ProjectName.Library` folder (based on the output filename stem).
- Rewrites saved media references to point at those copied library files.
- If different source files share the same filename, UltimateSlice keeps the first name and adds deterministic suffixes for collisions.
- Shows an export progress window while copying media and writing the packaged project XML.

When you open an existing FCPXML and save it without making edits, UltimateSlice preserves the original document verbatim so unknown attributes/fields from other tools are retained.
For edited saves, unsupported `asset-clip` attributes/child tags and imported resource `<asset>` metadata payloads (including nested `<metadata><md .../></metadata>`) are still carried forward in regenerated output. Unknown attrs/child tags are also preserved across core document structure (`<fcpxml>`, `<resources>`, selected `<library>/<event>/<project>/<sequence>/<spine>`, plus selected sequence `<format>` attrs) during dirty-save regeneration. Regenerated documents include `<!DOCTYPE fcpxml>` and keep source references in nested `<media-rep>` entries rather than legacy `asset@src`.
When possible for imported projects, transform-only dirty saves patch matching `adjust-transform` nodes in-place on the original XML (including nested clips), which keeps the saved file much closer to the source structure and IDs.
For transform position compatibility, UltimateSlice converts FCPXML `adjust-transform@position` values using frame-height percentage semantics (both X and Y percentages based on frame height, center-origin) to and from UltimateSlice's internal scale-aware position model (with Y-axis inversion) during import/export.
For FCPXML assets that use absolute source-time domains, UltimateSlice rebases `asset-clip@start` against `asset@start` during import so layered lane clips seek/play from the intended media-relative source position.
When imported FCPXML media references start with `/Volumes/...` and are missing locally, UltimateSlice URI-decodes the path (for example `%20` → space), retries common Linux external-drive mount locations (`/media/<user>/...`, `/run/media/<user>/...`, `/media/...`, `/run/media/...`, `/mnt/...`) plus the opened FCPXML mount-root fallback, then uses the found file for runtime playback while still saving the original imported XML source path.

## Auto-Save

UltimateSlice auto-saves every 60 seconds to `/tmp/ultimateslice-autosave.fcpxml` when the project has unsaved changes. This is a safety net — use **Save…** for permanent storage.
