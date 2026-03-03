# Preferences

The **Preferences** window contains application-level settings (not per-project settings).

## Opening Preferences

- Press **`Ctrl+,`** from the main window.

## Categories

Preferences are grouped by category in a sidebar:

- **General** — application information and credits.
- **Playback** — performance-related settings.
- **Timeline** — timeline display and analysis overlays.

## About & Open-source Credits (General)

- In **General**, click **About & Open-source credits** to open the About dialog.
- The dialog lists major third-party crates/libraries used by UltimateSlice and their license families.
- It also includes a license notice and pointers to `Cargo.toml`, `Cargo.lock`, and the Flatpak manifest for full dependency/license details.

## Hardware Acceleration (Playback)

- **Enable hardware acceleration** toggles the saved preference value and applies immediately to **source preview playback**.
- The setting is persisted across launches and available via MCP automation.
- Scope today:
  - affects source preview decode-mode selection (hardware-fast path when VA decoders are available, software-filtered fallback otherwise),
  - hardware-path errors automatically downgrade the current source to software decode for stability,
  - does not change export behavior.

## Program Monitor Playback Priority

- **Program monitor playback priority** controls how playback trades off smoothness vs precision:
  - `Smooth` (default): favors continuity and lower stutter during active playback.
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

## Proxy Preview Mode

- **Proxy preview mode** generates lightweight proxy files for smoother preview playback with large/high-bitrate media:
  - `Off` (default): uses original source media.
  - `Half Res`: generates half-resolution H.264 proxies.
  - `Quarter Res`: generates quarter-resolution H.264 proxies.
- When Proxy mode is `Off`, UltimateSlice now auto-enables proxies during heavy live-preview regions (3+ overlapping video tracks) to keep playback responsive, then returns to original media when overlap drops below that threshold.
- Auto-enabled proxy scale follows current preview pressure: Half-res by default, Quarter-res when preview quality is reduced to Quarter.
- Proxy files are transcoded in the background via ffmpeg and prefer a managed local cache root at `$XDG_CACHE_HOME/ultimateslice/proxies` (fallback `/tmp/ultimateslice/proxies`) for better external-drive playback.
- If local-cache writes/transcodes fail, UltimateSlice falls back to alongside-media `UltimateSlice.cache/` for that source.
- Managed local proxy cache entries are pruned at startup when stale (older than 24h by ownership index), and project unload/app close performs managed-cache cleanup.
- A yellow progress bar appears at the bottom of the window during proxy generation.
- **Changing the proxy size** (e.g. from Half Res to Quarter Res) automatically invalidates existing proxies and re-generates them at the new resolution.
- **LUT-baked proxies**: when a LUT is assigned to a clip via the Inspector, a new proxy is generated for that clip with the LUT baked in, so the preview reflects the color grade. Removing the LUT regenerates a plain (ungraded) proxy.
- Source Monitor behavior in `Off` mode is adaptive: it may request **Quarter** proxies for small source-monitor sizes and **Half** proxies for larger ones to improve preview smoothness.
- Proxy transcodes are tuned for fast preview decode (favoring playback smoothness over archival efficiency).
- Export always uses original full-resolution media regardless of proxy mode.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `proxy_mode`.
  - `set_proxy_mode` updates the mode and re-generates proxies as needed.

## Preview Quality (Playback)

- **Preview quality** scales down the compositor output resolution for preview playback:
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

## Saving

- Click **Save** to persist changes.
- Click **Cancel** to discard changes.

## Timeline Preview (Timeline)

- **Show timeline preview** controls video thumbnail rendering in the timeline:
  - Enabled (default): show the regular thumbnail strip across each visible video clip.
  - Disabled: only show start/end thumbnails for each visible video clip.
- Use Disabled mode to reduce thumbnail-generation workload on heavy media/projects.
- The setting is persisted across launches.

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
