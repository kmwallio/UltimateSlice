# Getting Started

## Requirements

- **Rust** (stable, via `rustup`)
- **GTK4** development libraries
- **GStreamer** plugins: core, base, good, bad, libav
- **ffmpeg** (for export — must be on `$PATH`)

### Linux (Ubuntu / Debian)

The default feature set builds `whisper-rs` from source, so install the compiler toolchain, `pkg-config`, and `cmake` alongside the media libraries:

```bash
sudo apt install \
  build-essential \
  cmake \
  pkg-config \
  libgtk-4-dev \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-libav \
  ffmpeg
```

### macOS (Homebrew)

Install [Homebrew](https://brew.sh) if you haven't already, then install the Xcode Command Line Tools:

```bash
xcode-select --install
```

After that, install the Homebrew packages (the default speech-to-text feature needs `cmake` during `cargo build`):

```bash
brew install cmake \
  gtk4 \
  gstreamer \
  gst-plugins-base \
  gst-plugins-good \
  gst-plugins-bad \
  gst-libav \
  ffmpeg
```

After installing, tell `cargo` where to find the pkg-config metadata. Add the following to your shell profile (e.g. `~/.zshrc`):

```bash
export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:$(brew --prefix)/share/pkgconfig"
```

Then reload your shell (`source ~/.zshrc`) before running `cargo build`.

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

When launched without a project file, UltimateSlice shows a **Welcome screen** with:
- **New Project** and **Open Project...** buttons
- A **Recent Projects** list (up to 10 entries with modification dates)
- A tip to get started

Clicking any action transitions smoothly to the editor. If you launch with a project file (e.g., `ultimate-slice project.uspxml`), the welcome screen is skipped and the project opens directly.

UltimateSlice uses a dark, GTK4/libadwaita-inspired control style so popovers, sliders, dropdowns, and tab groups remain visually consistent across panels.

Short-lived feedback now appears as an in-app toast near the top of the window instead of temporarily replacing the window title. Continuous work now has two layers: the bottom status bar gives a compact at-a-glance summary, and the footer **Jobs** dropdown beside **Workspace** expands active background tasks such as proxy generation, render work, exports, subtitle generation, and motion tracking into a detailed live list.

Hover compact icon, glyph, and swatch controls in the toolbar and Inspector to see descriptive tooltips before clicking; this is especially helpful for reorder buttons, style toggles, and color pickers.

Empty panels now try to tell you what to do next instead of just sitting blank: the Timeline shows a drop target when no clips exist yet, the Media Library centers its import action on a fresh project, and the Inspector explains how to select something to edit.

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
- Open **Preferences** (`Ctrl+,`) and use the **General** page's **About & Open-source credits** button to view third-party crate/library credits and license notices.
- Use **Save…** (`Ctrl+S`) to save as project XML at any point (default filename: `.uspxml`; `.fcpxml` also supported).

## Opening an Existing Project

- Click **Open…** (`Ctrl+O`) and select a `.uspxml` or `.fcpxml` file (the chooser also allows generic `.xml` fallback).
- Or click **Recent ▾** to pick from up to 10 opened/saved projects that still exist on disk.
- You can also launch UltimateSlice with a project file path argument (for example, `ultimate-slice /path/to/project.uspxml`) to open it immediately at startup.
- On Linux desktop environments, `.uspxml` files are registered as UltimateSlice project files and can be associated with the app.
- UltimateSlice reads FCPXML versions 1.10 through 1.14, including all clip properties, markers, and effects.
- For FCPXML files containing multiple projects, UltimateSlice imports the first project timeline in the file.
- Project file read/parse runs off the GTK main thread, so the UI remains responsive while opening larger timelines.

## Keyboard Shortcuts

See [shortcuts.md](shortcuts.md) for the full reference.  
Press **?** or **/** in the timeline to open the in-app shortcut overlay.
Use **Ctrl+,** to open **Preferences**.
