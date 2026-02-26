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

## Saving

- Click **Save** to persist changes.
- Click **Cancel** to discard changes.
