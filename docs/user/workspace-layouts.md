# Workspace Layouts

Workspace layouts let you save and restore app-wide panel arrangements for different tasks without changing project content.

## Open the Workspace menu

- Use the **Workspace** button in the bottom status bar.
- The popover includes:
  - **(Current)** for the live unsaved arrangement
  - **Default Layout** to restore the built-in arrangement
  - saved named layouts
  - **Apply**, **Save Current…**, **Rename…**, and **Delete**

## What gets saved

Workspace layouts capture the current editing arrangement, including:

- main splitter positions
- splitter restores scale to the current window size so maximized layouts reopen sensibly in smaller floating windows
- Media Browser visibility
- Inspector visibility
- Keyframe editor visibility and split position
- selected left-side tab (**Media**, **Effects**, **Audio Effects**, or **Titles**)
- Program Monitor scopes visibility and docked split position
- Program Monitor docked vs popped-out state
- Program Monitor pop-out window size

Saved layouts are stored in the app UI state, so they are available across projects on the same machine/account.

## Save, apply, rename, delete, and reset

1. Arrange the window the way you want.
2. Open **Workspace** in the status bar.
3. Click **Save Current…** and enter a name such as `Editing`, `Review`, or `Color`.
4. Select a saved layout and click **Apply** to restore it later.
5. Use **Rename…** or **Delete** for saved layouts, or choose **Default Layout** and click **Apply** to reset the window.

If you change the arrangement after applying a saved layout, the menu returns to **(Current)** until the layout matches a saved one again or you save a new preset.

Reserved built-in names such as `Current` and `Default Layout` cannot be used for saved layouts.

## What does not get saved

Workspace layouts do **not** include:

- project or timeline content
- playhead position or selection
- preview quality, proxy mode, prerender quality, or other playback/render preferences

Those remain separate so switching layouts only changes the workspace arrangement.

## MCP Automation

Workspace layouts are also available through MCP:

- `list_workspace_layouts`
- `save_workspace_layout`
- `apply_workspace_layout`
- `rename_workspace_layout`
- `delete_workspace_layout`
- `reset_workspace_layout`

See [python-mcp.md](python-mcp.md) for command examples.
