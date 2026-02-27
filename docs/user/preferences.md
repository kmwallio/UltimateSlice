# Preferences

The **Preferences** window contains application-level settings (not per-project settings).

## Opening Preferences

- Press **`Ctrl+,`** from the main window.

## Categories

Preferences are grouped by category in a sidebar:

- **General** — placeholder for upcoming general settings.
- **Playback** — performance-related settings.

## Hardware Acceleration (Playback)

- **Enable hardware acceleration** toggles the saved preference value and applies immediately to **source preview playback**.
- The setting is persisted across launches and available via MCP automation.
- Scope today:
  - affects source preview sink selection (`glsinkbin` path when enabled, `gtk4paintablesink` path when disabled),
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

## Proxy Preview Mode

- **Proxy preview mode** generates lightweight proxy files for smoother preview playback with large/high-bitrate media:
  - `Off` (default): uses original source media.
  - `Half Res`: generates half-resolution H.264 proxies.
  - `Quarter Res`: generates quarter-resolution H.264 proxies.
- Proxy files are transcoded in the background via ffmpeg and stored in `.ultimateslice_proxies/` next to the source files.
- A yellow progress bar appears at the bottom of the window during proxy generation.
- **Changing the proxy size** (e.g. from Half Res to Quarter Res) automatically invalidates existing proxies and re-generates them at the new resolution.
- **LUT-baked proxies**: when a LUT is assigned to a clip via the Inspector, a new proxy is generated for that clip with the LUT baked in, so the preview reflects the color grade. Removing the LUT regenerates a plain (ungraded) proxy.
- Export always uses original full-resolution media regardless of proxy mode.
- The setting is persisted across launches.
- MCP automation:
  - `get_preferences` returns `proxy_mode`.
  - `set_proxy_mode` updates the mode and re-generates proxies as needed.

## Saving

- Click **Save** to persist changes.
- Click **Cancel** to discard changes.

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
- The setting is persisted across launches.
