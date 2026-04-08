# PNG preview / transform issue summary

> **Status:** This issue is still **not fully solved**. The fixes listed below describe work that landed, but PNG visibility during Program Monitor playback/transform editing is still unreliable and needs more debugging.

## Sample project

- Repro project: `/home/kmwallio/PNG.uspxml`

## Symptoms

1. PNG clips could disappear from the Program Monitor after reopen or during live preview updates.
2. The transform overlay could be misaligned for PNGs when the underlying base video had a different aspect ratio.
3. PNGs attached to tracker-driven overlay work exposed shared transform issues around `Scale = 1.0`.
4. Static PNGs could disappear during playback, paused reseeks, or transform/crop drags until some later redraw happened.

## Root causes

1. **Image-kind persistence/import gap**  
   Reopened still-image clips could lose `ClipKind::Image`, which made Program Monitor treat them like normal time-varying video instead of held stills.

2. **Wrong inset source for transform overlay**  
   The transform overlay used the first active video slot's letterbox/pillarbox inset instead of the selected PNG clip's own preview framing.

3. **Shared scale-dependent motion math**  
   The preview/export/overlay motion path reused scale-sensitive pan math that was fine for normal media placement but wrong for tracker-followed titles/overlays at `Scale = 1.0`.

4. **Static-image source time advanced with the playhead**  
   Live preview reseeks treated PNGs like real time-based media and advanced their source position instead of pinning them to the still frame.

5. **Static PNG drags used the generic live transform mode**  
   The live transform path was tuned for moving video, but static PNG edits needed a reliable paused refresh path to keep the compositor fed with a fresh still frame.

## Fixes that landed

1. FCPXML/`.uspxml` load now infers still-image clips from the source path, and save persists `us:clip-kind="image"` so reopened PNGs stay `ClipKind::Image`.
2. Program Monitor transform-overlay alignment now uses the selected clip's own preview inset.
3. Titles, adjustment layers, and tracker-followed overlays now use direct canvas translation so tracked/manual `Position X/Y` still works at `Scale = 1.0`.
4. Normal still-image clips stay on the existing still-image preview path unless they are actually following a tracker.
5. Live preview reseeks pin static PNGs to their `source_in` frame instead of advancing through nonexistent media time.
6. Static PNG transform/crop drags bypass the generic live transform mode and use the normal paused refresh path so the PNG stays visible while editing.

## Current intended behavior

- **Note:** this is the target behavior, not a statement that the bug is fully resolved yet.
- Static PNG/JPEG/WebP/SVG overlays should stay visible:
  - after reopen,
  - during playback,
  - during paused reseeks,
  - while moving, scaling, or cropping in the Program Monitor.
- Tracker-followed PNG/title overlays should still use the direct-follow motion path.

## Files most directly involved

- `src/fcpxml/parser.rs`
- `src/fcpxml/writer.rs`
- `src/media/program_player.rs`
- `src/ui/window.rs`
- `docs/user/program-monitor.md`
