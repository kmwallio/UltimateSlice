# Preferences

The **Preferences** window contains application-level settings (not per-project settings).
Selector controls in this window use a single control frame (no nested double-border styling).

## Opening Preferences

- Press **`Ctrl+,`** from the main window.

## Categories

Preferences are grouped by category in a sidebar:

- **General** — application information and credits.
- **Playback** — performance-related settings.
- **Proxies** — proxy generation, preview LUTs, background prerender, and cache persistence.
- **Timeline** — timeline display and analysis overlays.
- **Integration** — MCP socket-server connectivity.
- **Models** — downloadable AI model assets when the build includes AI inference support.

## About & Open-source Credits (General)

- In **General**, click **About & Open-source credits** to open the About dialog.
- The dialog lists major third-party crates/libraries used by UltimateSlice and their license families.
- It also includes a license notice and pointers to `Cargo.toml`, `Cargo.lock`, and the Flatpak manifest for full dependency/license details.

## Auto-Backup (General)

| Setting | Type | Default | Description |
|---|---|---|---|
| **Auto-backup** | Checkbox | On | Create timestamped backup copies every 60 seconds when the project has unsaved changes |
| **Max backup versions** | Spin (1–100) | 20 | Maximum versioned backups per project title; oldest are pruned |

Backups are stored in `~/.local/share/ultimateslice/backups/` (or `$XDG_DATA_HOME/ultimateslice/backups/`). See [project-settings.md](project-settings.md#versioned-backups) for restore instructions.

## Hardware Acceleration (Playback)

- **Enable hardware acceleration** toggles the saved preference value and applies immediately to **source preview playback**.
- The setting is persisted across launches and available via MCP automation.
- Scope today:
  - affects source preview decode-mode selection (hardware-fast path when VA decoders are available, software-filtered fallback otherwise),
  - hardware-path errors automatically downgrade the current source to software decode for stability,
  - does not change export behavior.

## Program Monitor Playback Priority

- **Program monitor playback priority** controls how playback trades off smoothness vs precision:
  - `Smooth` (default): favors continuity and lower stutter during active playback. Under load, Program Monitor may drop late video frames to stay closer to the audio clock/playhead instead of building preview latency.
  - `Balanced`: middle ground.
  - `Accurate`: favors precise seeks/frame placement.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `playback_priority`.
  - `set_playback_priority` updates the mode.

## Source Monitor Playback Priority

- **Source monitor playback priority** controls Source Monitor seek behavior:
  - `Smooth`: prefer lighter keyframe seeks for responsive playback/scrub.
  - `Balanced`: same behavior as Smooth today (reserved for future tuning).
  - `Accurate`: prefer frame-accurate seek behavior.
- Frame-step actions remain frame-accurate regardless of this setting.
- MCP automation:
  - `get_preferences` returns `source_playback_priority`.
  - `set_source_playback_priority` updates the mode.

## Proxy Preview Mode (Proxies)

- **Proxy preview mode** generates lightweight proxy files for smoother preview playback with large/high-bitrate media:
  - `Off` (default): uses original source media and does not request/generate proxy files.
  - `Half Res`: generates half-resolution H.264 proxies.
  - `Quarter Res`: generates quarter-resolution H.264 proxies.
- The bottom status bar includes a proxy state toggle next to the render-mode toggle. It reads **Using Proxies** when proxy playback is active and **Original Media** when it is off, and **`Shift+P`** provides the same quick on/off action without opening Preferences.
- Turning the quick toggle back on restores the last non-`Off` proxy size you chose here, so the fast toggle keeps using your preferred `Half Res` or `Quarter Res` setting.
- Proxy files are transcoded in the background via ffmpeg and prefer a managed local cache root at `$XDG_CACHE_HOME/ultimateslice/proxies` (fallback `/tmp/ultimateslice/proxies`) for better external-drive playback.
- While a proxy is still incomplete/unusable, UltimateSlice keeps playback on original media and switches to the proxy only after it is valid.
- If local-cache writes/transcodes fail, UltimateSlice falls back to alongside-media `UltimateSlice.cache/` for that source.
- When **Persist proxies next to original media** is enabled and Proxy mode is on, successful local proxy transcodes are mirrored into alongside-media `UltimateSlice.cache/` for reuse.
- Managed local proxy cache entries are pruned at startup when stale (older than 24h by ownership index).
- Proxy file names stay stable for the same source path and proxy-affecting variant state (resolution, LUT, stabilization), so reopening a project can reuse an existing proxy instead of regenerating it unnecessarily.
- UltimateSlice stores source-signature metadata beside each proxy and automatically regenerates it when the source media at that path changes on disk.
- When older `UltimateSlice.cache/<stem>.proxy_*` sidecar files already exist, UltimateSlice reuses those legacy proxy names too instead of needlessly re-encoding them.
- When the current project's proxy expectations change, UltimateSlice automatically removes stale/superseded current-format proxy variants and leftover `.partial` files from the managed local cache and the matching `UltimateSlice.cache/` directories for that project's sources.
- On project unload/app close, UltimateSlice always cleans tracked proxy files from managed local cache (`$XDG_CACHE_HOME`/`/tmp`).
- Alongside-media `UltimateSlice.cache/` proxies are preserved only when both **Proxy mode** and **Persist proxies next to original media** are enabled; otherwise those tracked sidecar proxies are cleaned on unload/close.
- When Proxy mode is enabled, project reload eagerly primes a capped set of near-playhead proxy sources so first playback can pick up local proxies sooner on slower/external storage.
- A yellow progress bar appears at the bottom of the window during proxy generation (and now also when background timeline prerender jobs are in flight).
- Proxy percentage now uses ffmpeg bytes-written (`total_size`) versus a bitrate×duration estimate, and remains below 100% while jobs are still running.
- **Changing the proxy size** (e.g. from Half Res to Quarter Res) automatically invalidates existing proxies and re-generates them at the new resolution.
- **LUT-baked proxies**: when a LUT is assigned to a clip via the Inspector, a new proxy is generated for that clip with the LUT baked in, so the preview reflects the color grade. Removing the LUT regenerates a plain (ungraded) proxy.
- Source Monitor follows Proxy mode strictly: in `Off` mode it loads original media and does not request proxies.
- Proxy transcodes are tuned for fast preview decode (favoring playback smoothness over archival efficiency).
- Export always uses original full-resolution media regardless of proxy mode.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `proxy_mode`, `last_non_off_proxy_mode`, and `persist_proxies_next_to_original_media`.
  - `set_proxy_mode` updates the mode, preserves the last non-`Off` proxy size for quick restore, and re-generates proxies as needed.

## Proxy Sidecar Persistence (Proxies)

- **Persist proxies next to original media** controls whether reusable proxy files are mirrored into `UltimateSlice.cache/` beside the source media.
- Enabled (default): keep proxy sidecars next to the source media so reopened projects can reuse them from the media drive.
- Disabled: prefer the managed local cache only, and clean tracked proxy sidecars on unload/close.
- Local-cache failure fallback still uses alongside-media cache paths when needed for reliability.
- MCP automation:
  - `get_preferences` returns `persist_proxies_next_to_original_media`.
  - `set_proxy_sidecar_persistence` toggles the setting.

## Preview LUTs (Proxies)

- **Preview LUTs (Proxy Off mode)** pre-renders project-resolution preview media for clips that have a LUT assigned when Proxy mode is `Off`.
- This keeps LUT-heavy timelines smoother without requiring global proxy mode.
- When Proxy mode is enabled (`Half Res` or `Quarter Res`), normal proxy behavior takes precedence.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `preview_luts`.
  - `set_preview_luts` toggles the setting.

## Preview Quality (Playback)

- **Preview quality** scales down Program Monitor preview processing resolution (effects/compositor) and output resolution for preview playback:
  - `Auto`: adapts quality to the current Program Monitor canvas size.
  - `Full` (default): renders at project resolution (e.g. 1920×1080).
  - `Half`: halves both dimensions (e.g. 960×540) — 4× fewer pixels, significantly less memory and CPU.
  - `Quarter`: quarters both dimensions (e.g. 480×270) — 16× fewer pixels, best for low-memory devices.
- Reduced preview quality keeps the same framing as Full quality: the full composed frame is scaled to fit the monitor (no top-left cropping).
- Takes effect immediately — no restart required.
- Export always uses full project resolution regardless of this setting.
- Combine with **Proxy preview mode** for maximum performance on constrained hardware: Quarter-res proxies + Quarter preview quality minimizes both decode and compositing cost.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `preview_quality`.
  - `set_preview_quality` updates the quality level (`auto`, `full`, `half`, `quarter`).

## Experimental Preview Optimizations (Playback)

- **Experimental preview optimizations** enables an occlusion optimization path for multi-track playback.
- When enabled, clips fully hidden behind opaque full-frame clips can use audio-only decode paths during preview.
- This can reduce decode load on heavy overlaps; visual output remains driven by visible clips.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `experimental_preview_optimizations`.
  - `set_experimental_preview_optimizations` toggles the setting.

## Real-time Preview (Playback)

- **Real-time preview** pre-builds upcoming decoder slots so boundary transitions are faster.
- This can improve responsiveness at clip boundaries but may increase CPU/memory usage.
- **Enabled by default.** Disabling it forces full pipeline teardown/rebuild at every clip boundary, which can cause ~500ms stutter per transition.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `realtime_preview`.
  - `set_realtime_preview` toggles the setting.

## Background Prerender (Proxies)

- **Background prerender** pre-renders complex upcoming overlap sections (3+ active video tracks) to disk clips in the background.
- You can quickly toggle it from the bottom status bar next to **Track Audio Levels** without opening Preferences. The button reads **Background Render** when prerendering is enabled and **Live Rendering** when it is disabled.
- The toggle uses run/stop symbolic icons (`process-stop-symbolic` when off, `system-run-symbolic` when on) to make state visible at a glance.
- When available, Program Monitor playback can use the prerendered section clip instead of rebuilding all video layers live for that segment.
- If both **Real-time preview** and **Background prerender** are enabled, 3+ track overlap boundaries now prefer the prerender-capable path so prerender clips are still used during full playthrough.
- Prerender playback uses the same preview-processing dimensions as live playback, so reduced Preview Quality modes do not crop prerender output to a top-left region.
- Animated **brightness / contrast / saturation / temperature / tint** keyframes are now preserved in prerendered overlap segments, keeping heavy-overlap preview closer to export when those color controls are animated.
- If a prerender segment finishes while playback is already inside that overlap region, UltimateSlice can now switch into the prerender path mid-segment (via a short rebuild) instead of waiting for the next boundary.
- While paused or stopped, UltimateSlice also schedules nearby prerender jobs around the current playhead so heavy sections can be ready before playback starts.
- While playing, background prerender scheduling is bounded to upcoming boundaries (not every playhead tick) to reduce job churn and keep readiness stable.
- While currently inside a prerendered overlap segment, UltimateSlice prewarms the immediate boundary after that segment so post-prerender preview playback is ready sooner.
- Prototype path: prerender segments currently include mixed audio, and prerender playback can run as a single prerender decoder branch (video + audio) during heavy overlap sections.
- Prerender segment duration now covers the full overlap span to the next boundary (not a fixed ~4s chunk), reducing black tails when long overlap regions are active.
- When Proxy mode is enabled, background prerender segments render at the active proxy scale (Half/Quarter) for faster prerender generation.
- Prerender activity is surfaced in the existing bottom status/progress bar used for proxy generation.
- Only active when enabled in Preferences.
- When **Persist prerenders next to project file** is enabled, saved projects keep prerender cache files in a sibling `UltimateSlice.cache/prerender-vN/<project-hash>/` directory and startup/open preserves that saved-project cache root so reopening the same project can reuse compatible prerender segments.
- Completed prerender jobs are written atomically through a temporary MP4 file before the final rename into the cache, so successful overlap renders now actually land in the prerender cache instead of failing on the temporary filename.
- Unsaved/new projects still use the temporary prerender cache root until the project has a stable save path, and disabling the persistence setting keeps saved projects on that temporary-only path too.
- Reuse is validated with a prerender manifest that records the contributing source/proxy files and their size/mtime signatures; changed inputs invalidate the cached segment automatically.
- Disabling **Background prerender** clears the current project's prerender cache files.
- If a prerender boundary clip fails to link reliably, UltimateSlice automatically falls back to the normal live rebuild path for stability.
- Animated transform/mask properties, speed/reverse/freeze timing, and advanced clip-audio animation still fall back to the normal live path when prerender cannot reproduce them safely.
- When a boundary is not warm, playback falls back to the normal live rebuild path.
- Uses more CPU/memory while playing and is disabled by default.
- MCP automation:
  - `get_preferences` returns `background_prerender` and `persist_prerenders_next_to_project_file`.
  - `set_background_prerender` toggles the setting.

## Prerender Cache Persistence (Proxies)

- **Persist prerenders next to project file** controls whether saved projects keep reusable prerender segments in a sibling `UltimateSlice.cache/prerender-vN/` directory.
- Enabled (default): saved projects can reuse compatible prerenders after reopen.
- Disabled: prerender output stays in the temporary cache root and is treated as session-local scratch data.
- Unsaved projects always use the temporary cache root until they have a stable file path.
- MCP automation:
  - `get_preferences` returns `persist_prerenders_next_to_project_file`.
  - `set_prerender_project_persistence` toggles the setting.

## Saving

- Click **Save** to persist changes.
- Click **Cancel** to discard changes.

## Timeline Preview (Timeline)

- **Show timeline preview** controls video thumbnail rendering in the timeline:
  - Enabled (default): show the regular thumbnail strip across each visible video clip.
  - Disabled: only show start/end thumbnails for each visible video clip.
- Use Disabled mode to reduce thumbnail-generation workload on heavy media/projects.
- The setting is persisted across launches.

## Audio Crossfades (Timeline)

- **Enable automatic audio crossfades at edit points** toggles automatic crossfades for adjacent same-track timeline edits during Program Monitor playback and export.
- **Crossfade curve** chooses `Equal power` (default) or `Linear`.
- **Crossfade duration (ms)** sets the target fade duration (stored internally as nanoseconds for playback/export compatibility).
- Fade windows are automatically clamped for short adjacent clips, so crossfades cannot exceed half of either clip.
- Settings are persisted across launches with backward-compatible defaults for older config files.
- MCP automation:
  - `get_preferences` returns `crossfade_enabled`, `crossfade_curve`, and `crossfade_duration_ns`.
  - `set_crossfade_settings` updates these values with strict validation (`curve`: `equal_power`/`linear`, `duration_ns`: 10_000_000–10_000_000_000).

## GTK Renderer (Playback)

- **GTK renderer** controls which graphics backend GTK uses to draw the application window:
  - `Auto` (default): let GTK decide (usually Vulkan on supported systems).
  - `Cairo (Software)`: CPU-based rendering — uses no GPU memory at all. Best for devices with limited GPU memory that see `VK_ERROR_OUT_OF_DEVICE_MEMORY` errors.
  - `OpenGL`: moderate GPU memory usage — a good middle ground.
  - `Vulkan`: explicit Vulkan rendering — highest quality, highest GPU memory usage.
- **Requires a restart** to take effect (the renderer is selected before GTK initializes).
- Export is unaffected — it always uses ffmpeg regardless of the renderer setting.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `gsk_renderer`.
  - `set_gsk_renderer` updates the mode (restart required to apply).

## MCP Socket Server (Integration)

- **Enable MCP socket server** allows AI agents to connect to this running instance via a Unix domain socket.
- When enabled, UltimateSlice listens at `$XDG_RUNTIME_DIR/ultimateslice-mcp.sock`.
- The toggle takes effect immediately — no restart required.
- Only one agent can be connected at a time; additional connections are rejected.
- Agents using the `.mcp.json` `ultimate-slice-attach` server entry connect via `--mcp-attach`, which bridges stdio to the socket.
- A Python socket client is also available: `python3 tools/mcp_socket_client.py` (see `docs/user/python-mcp.md`).
- The setting is persisted across launches.
