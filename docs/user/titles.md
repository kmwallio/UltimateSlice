# Titles

UltimateSlice includes a built-in Titles system for adding text overlays and standalone title cards to your timeline.

## Titles Browser

The **Titles** tab appears in the left panel alongside Media and Effects. It lists 9 built-in title templates organized by category:

### Standard
- **Lower Third (Banner)** -- Semi-transparent dark banner near the bottom
- **Lower Third Clean** -- Clean text with outline, no banner
- **Centered Title** -- Large bold text centered with drop shadow
- **Subtitle** -- Small text at the very bottom with background box

### Cinematic
- **Full Screen** -- Very large centered text on solid black background
- **Chapter Heading** -- Outline + shadow serif heading
- **Cinematic** -- Large serif text with outline and shadow

### Informational
- **End Credits** -- Centered text on solid black, supports secondary line
- **Callout** -- Yellow box with black text for callouts

### Adding Titles

1. Select a template in the Titles browser
2. Click **"Add to Timeline"** to create a standalone title clip at the playhead position
3. Double-click a template for quick add

### Applying to Existing Clips

1. Select a clip on the timeline
2. Select a template in the Titles browser
3. Click **"Apply to Clip"** to apply the template's styling to the selected clip's title overlay

Use the search bar to filter templates by name, description, or category.

## Title Clip Properties

Standalone title clips (`ClipKind::Title`) are placed on video tracks like any other clip. They have:
- **No source media** -- rendered as a solid color or transparent background
- **Warm gold color** on the timeline for easy identification
- **"T" badge** and centered text label in the timeline view
- **Any duration** -- drag to resize like image clips

## Title Styling (Inspector)

The Inspector's **Title Overlay** section provides full control over title text appearance:

### Text & Position
- **Text entry** -- the title text to display
- **Font** -- click to choose a font (Pango font description)
- **Text Color** -- color picker with alpha support
- **Position X/Y** -- relative position (0.0--1.0)

### Outline
- **Outline Width** -- stroke width in points (0 = no outline)
- **Outline Color** -- stroke color with alpha

### Drop Shadow
- **Drop Shadow** checkbox -- enable/disable
- **Shadow Color** -- shadow color with alpha
- **Shadow Offset X/Y** -- shadow offset in points (-10 to 10)

### Background Box
- **Background Box** checkbox -- enable/disable
- **Box Color** -- background box color with alpha
- **Box Padding** -- padding around text in points (0--30)

## Preview vs Export

Title text renders in both the GStreamer preview pipeline and FFmpeg export. The preview uses GStreamer's `textoverlay` element which approximates some effects:
- Outline width is fixed at ~1px in preview (pixel-accurate in export)
- Shadow offset is fixed in preview (configurable in export)
- Background box color is fixed dark in preview (exact color in export)

Export uses FFmpeg's `drawtext` filter which supports all styling options at full fidelity.

## MCP Tools

- `add_title_clip` -- Create a title clip from a template
  - Required: `template_id`
  - Optional: `track_index`, `timeline_start_ns`, `duration_ns`, `title_text`
- `set_clip_title_style` -- Set title styling on any clip
  - Required: `clip_id`
  - Optional: all title properties (text, font, color, position, outline, shadow, bg box, etc.)

## FCPXML Persistence

All title properties are saved as `us:title-*` vendor namespace attributes in FCPXML/USPXML files. Title clips are identified by `us:clip-kind="title"`. Backward compatible -- older projects without title fields load with sensible defaults.
