# Site Changelog

This file tracks updates made to the UltimateSlice website and documentation, specifically those synchronized from the `main` development branch.

## [2026-03-03] - Update from Main
**Synchronized with main branch commit:** `c4d36f0`

### Added
- **Reverse Playback**:
  - Documented per-clip "Reverse" toggle in Inspector and Program Monitor.
  - Added description for the timeline `◀` badge.
  - Updated Export documentation to include `reverse` and `areverse` filter details.
- **Experimental Preview Optimizations**:
  - Documented occlusion-based video decode skip and continuous decoders in Preferences.
  - Added real-time preview prioritized playback option.
- **MCP Tools**:
  - **Program Monitor**: Documented `play`, `pause`, and `stop` tools.
  - **Source Monitor**: Documented `select_library_item`, `source_play`, `source_pause`, `set_source_in`, and `set_source_out`.
  - **Preferences**: Documented tools for proxy mode, preview quality, GSK renderer, and real-time preview settings.

### Changed
- **Project Serialization**:
  - Added support for `*.uspxml` format alongside standard FCPXML.
  - Documented the `application/x-ultimateslice-project+xml` MIME type association.
- **FCPXML Interchange**:
  - Documented improved compatibility for Apple-authored FCPXML files, chapter markers, and standard audio gain mapping.
- **Timeline**:
  - Updated Ruler documentation to reflect improved responsiveness for frame-accurate scrubbing.
- **Export**:
  - Documented overflow-aware clipping for secondary-track overlays (PIP) to ensure preview/export parity.

## [2026-03-01] - Update from Main
**Synchronized with main branch commit:** `2de6b60`

### Added
- **App Icon Integration**:
  - Added `io.github.kmwallio.ultimateslice.svg` as site favicon.
  - Added app icon to the global header next to the site title.
  - Integrated the app icon into hero sections on the Home, Features, and Credits pages.
  - Added `.site-logo` CSS class for consistent icon sizing.
- **Documentation for New Features**:
  - **Program Monitor**: Added VU meters, color scopes (Waveform, Histogram, RGB Parade, Vectorscope), J/K/L shuttle scrubbing, master volume slider, and transform overlay precision controls.
  - **Inspector**: Added Shadows/Midtones/Highlights grading, collapsible/context-sensitive sections, and per-clip volume control.
  - **Timeline**: Added color-coded waveforms (Green/Yellow/Red), waveform overlay on video clips, and new transition types (Fade to black, Wipe right, Wipe left).
  - **Media Library**: Added context-sensitive import button (hides when populated) and background media probing.
  - **Preferences**: Added "Show audio waveforms on video clips" setting and detailed hardware acceleration info.
  - **MCP Tools**: Documented new `take_screenshot` tool.
- **Licensing**: Added project license (GPL-3.0-or-later) details to the Credits page.

### Changed
- **Branding & Links**:
  - Updated Flatpak app-id to `io.github.kmwallio.ultimateslice` throughout the documentation.
  - Updated all GitHub repository links to point to `kmwallio/UltimateSlice`.
  - Updated `_config.yml` with the correct `github_username`.
  - Moved "Audio Waveforms", "Filmstrip Thumbnails", and "Real-time Effects" from the Roadmap to the Implemented Features list in `features.md`.

### Fixed
- **Source Monitor**: Fixed missing volume slider in the controls documentation.
- **Flatpak**: Updated documentation to reflect external drive permissions (`/run/media`).
