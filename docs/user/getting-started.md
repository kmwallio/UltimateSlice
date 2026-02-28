# Getting Started

## Requirements

- **Linux** (GTK4 + GStreamer stack)
- **Rust** (stable, via `rustup`)
- **GStreamer** plugins: `gstreamer`, `gstreamer-plugins-base`, `gstreamer-plugins-good`, `gstreamer-plugins-bad`
- **ffmpeg** (for export — must be on `$PATH`)

Install dependencies on Ubuntu/Debian:

```bash
sudo apt install \
  libgtk-4-dev \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-libav \
  ffmpeg
```

## Building & Running

```bash
git clone https://github.com/kmwallio/UltimateSlice.git
cd UltimateSlice
cargo run
```

For the MCP server mode (for AI agent control):

```bash
cargo run -- --mcp
```

## First Launch

The application opens with an **Untitled** project containing one Video track and one Audio track.

### Window Layout

```
┌──────────────────────────────────────────────────────────────────┐
│  Toolbar (New / Open / Recent / Save / Settings / Export / Undo / Redo)  │
├────────────────┬─────────────────────────────┬────────────────────┤
│  Media Library │   Source Monitor            │  Inspector         │
│                │   (source preview)          │  (clip properties) │
├────────────────┴─────────────────────────────┤                    │
│   Program Monitor (assembled timeline view)  │                    │
├──────────────────────────────────────────────┴────────────────────┤
│   Timeline (multi-track, ruler, clips)                            │
└───────────────────────────────────────────────────────────────────┘
```

## Creating a New Project

- Click **New** in the toolbar (or press `Ctrl+N`).
- Click **⚙ Settings** to configure canvas resolution and frame rate.
- Use **Save…** (`Ctrl+S`) to save as FCPXML at any point.

## Opening an Existing Project

- Click **Open…** (`Ctrl+O`) and select a `.fcpxml` file.
- Or click **Recent ▾** to pick from the last 10 opened/saved projects.
- UltimateSlice reads the FCPXML 1.10 format, including all clip properties, markers, and effects.
- Project file read/parse runs off the GTK main thread, so the UI remains responsive while opening larger timelines.

## Keyboard Shortcuts

See [shortcuts.md](shortcuts.md) for the full reference.  
Press **?** or **/** in the timeline to open the in-app shortcut overlay.
Use **Ctrl+,** to open **Preferences**.
