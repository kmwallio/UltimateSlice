# FCPXML Reference & Parsing Guide

This document provides a technical overview of the FCPXML (Final Cut Pro XML) format, its structure, key elements, and how to accurately parse its attributes. This guide is designed to be a functional blueprint for building an FCPXML-compatible video editor.

> **Scope in UltimateSlice**
>
> This file is a broad FCPXML format reference, not a full statement of current UltimateSlice feature parity for every tag listed below.
> UltimateSlice's currently implemented import/export subset is centered on:
> - FCPXML versions **1.10 through 1.14** (export writes `1.14`)
> - `format`, `asset`, nested `media-rep`, and `asset-clip` timeline structures
> - import fallback for spine `ref-clip` and `sync-clip` containers (nested clip item traversal)
> - marker + chapter-marker import
> - native spine `transition` import/export (mapped to UltimateSlice transition fields)
> - native `timeMap`/`timept` import/export for 2-point constant retimes plus representable multi-point monotonic ramps (mapped to speed keyframes), including `smooth2` easing mapping; unsupported imported maps (for example with `inTime`/`outTime`) are preserved and re-emitted
> - `adjust-transform`, `adjust-compositing`, and `adjust-crop`/`crop-rect` mappings used by Inspector fields
> - `adjust-volume` / `adjust-panner` keyframe import from `<audio-channel-source>` containers; strict export wraps keyframed volume/pan in `<audio-channel-source>` for FCP compatibility
> - per-asset format generation with embedded timecode extraction (video via ffprobe stream tags, audio via BWF `time_reference`)
> - audio-only asset handling with `FFVideoFormatRateUndefined` format, 48 kHz time base, and probed duration
> - connected clips nested inside primary storyline clips with offset in parent source time space
> - unknown-field preservation for imported FCPXML in clean-save and dirty-save flows

---

## 1. Overview
FCPXML is an XML-based interchange format used by Final Cut Pro to describe libraries, events, projects, and their contained media and timelines. 

**Key Concept: Definition vs. Usage**
FCPXML separates the **definition** of a resource (like a high-res video file or a specific frame rate) from its **usage** (how many seconds of that file appear in your edit). This allows you to change the source file once in the "Resources" section, and every instance of it in your timeline will update automatically.

---

## 2. Basic Structure & Projects
FCPXML follows a strictly hierarchical structure, mirroring the organization of a real-world video project.

### **Attribute Meanings**
*   **`version`**: The schema version (e.g., `1.10`, `1.11`, `1.12`, `1.13`, `1.14`). Version 1.10+ is required for many modern features like HDR and Cinematic mode. Version 1.13 corresponds to Final Cut Pro 11. Version 1.14 corresponds to Final Cut Pro 12.0.
*   **`format`**: A unique ID referencing a `<format>` resource. This defines the "canvas" (resolution) and "heartbeat" (frame rate) of the element.
*   **`tcStart`**: The starting timecode of the timeline. Usually `3600s` (01:00:00:00).
*   **`tcFormat`**: Determines if the clock skips frames to stay in sync with real-time (`DF` for Drop Frame) or counts every single frame (`NDF` for Non-Drop Frame).

### **The `<resources>` Dictionary**
FCPXML uses a central repository for all "heavy" objects. This avoids duplicating data when the same file or effect is used multiple times in a timeline.

*   **`<format>`**: Defines the canvas (resolution and frame rate). See above for details.
*   **`<asset>`**: Represents a physical file on disk (video, audio, or image).
    *   Contains one or more **`<media-rep>`** children that reference the actual file(s), plus optional `<metadata>`.
    *   **`start` / `duration`**: The intrinsic bounds of the file.
    *   **`hasVideo` / `hasAudio`**: Boolean flags (`1` or `0`).
    *   **`format`**: Reference to a `<format>` resource ID.
    *   **`audioSources`**, **`audioChannels`**, **`audioRate`**: Audio stream properties.
    *   **`videoSources`**: Number of video sources.
    *   **`customLUTOverride`**: Custom LUT or built-in log mode identifier.
    *   **`auxVideoFlags`**: Auxiliary video flags.
*   **`<media-rep>`**: A physical file reference within an `<asset>`. Each asset must have at least one.
    *   **`src`** (required): The file URL (e.g., `file:///media/clip.mov`).
    *   **`kind`**: `original-media` (default) or `proxy-media`.
    *   **`sig`**: File signature assigned by FCP.
    *   **`suggestedFilename`**: Filename hint (with extension) when the name shouldn't be derived from the URL.
    *   Can contain an optional `<bookmark>` child.
    > **Backward compatibility note:** Older FCPXML versions (pre-1.11) had `src` directly on `<asset>`. In v1.11+, `src` lives on `<media-rep>` children instead.
*   **`<media>`**: Represents "synthetic" or "nested" media, such as Multicam clips or Compound clips. It contains its own internal timeline.
*   **`<effect>`**: Defines a visual or audio filter, transition, or generator.
    *   **`uid`**: A unique string identifying the specific plugin (e.g., `.../Video Filters/Color/Color Board`).

### **Developer Example: Resource Interaction**
*This example shows how a sequence (r1) uses an asset (r2) and applies an effect (r3).*

```xml
<resources>
    <format id="r1" frameDuration="100/2400s" width="1920" height="1080"/>
    <asset id="r2" name="Interview" start="0s" duration="100s" hasVideo="1" hasAudio="1">
        <media-rep kind="original-media" src="file:///media/clip.mov"/>
    </asset>
    <effect id="r3" name="Color Board" uid=".../Video Filters/Color/Color Board"/>
</resources>

<sequence format="r1" duration="10s">
    <spine>
        <asset-clip ref="r2" offset="0s" duration="10s">
            <!-- Usage: Applying the 'Color Board' effect to this specific clip -->
            <filter-video ref="r3" name="Color Correction"/>
        </asset-clip>
    </spine>
</sequence>
```

### **Developer Example: Asset with Original + Proxy Media**
*An asset can have multiple `<media-rep>` children for different quality levels.*

```xml
<resources>
    <format id="r1" frameDuration="100/2400s" width="1920" height="1080"/>
    <asset id="r2" name="Interview_CamA" start="0s" duration="3600s"
           hasVideo="1" hasAudio="1" format="r1"
           audioSources="1" audioChannels="2" audioRate="48000">
        <media-rep kind="original-media" src="file:///media/camera_a.mov"/>
        <media-rep kind="proxy-media" src="file:///media/camera_a_proxy.mov"/>
    </asset>
</resources>
```

---

## 3. Advanced Timeline Structures

### Compound Media
The `<media>` resource allows for complex nesting. A **Compound Clip** is essentially a sequence inside a resource, allowing you to treat a complex edit as a single clip.

### **Developer Example: Compound Media**
```xml
<resources>
    <media id="r4" name="My Compound Clip">
        <sequence format="r1" duration="30s">
            <spine>
                <asset-clip ref="r2" offset="0s" duration="30s"/>
            </spine>
        </sequence>
    </media>
</resources>

<!-- In the main timeline, we reference the media 'r4' -->
<ref-clip ref="r4" offset="0s" duration="10s"/>
```

### Multicam Editing
A **Multicam clip** is a "stack" of synchronized camera angles. In the editor, you only see the "active" angle, but all angles remain perfectly synced in the background.

*   **`<multicam>`**: The container inside a `<media>` resource that defines the angles.
    *   **`format`** (required): Reference to a `<format>` resource ID.
    *   **`duration`**: Total duration.
    *   **`tcStart`**: Timecode origin.
    *   **`tcFormat`**: `DF` | `NDF`.
    *   **`renderFormat`**: Codec for preview renders.
*   **`<mc-angle>`**: A container for a single camera's footage. Each angle has a unique `angleID`.
*   **`<mc-clip>`**: The usage tag in the timeline that references a multicam resource.
    *   **`ref`** (required): Reference to a `<media>` ID containing a `<multicam>`.
    *   **`srcEnable`**: `all` (default) | `audio` | `video`. Controls which streams are active.
    *   **`audioStart` / `audioDuration`**: For split edits (J/L cuts).
    *   **`modDate`**: Modification date.
    > **Note:** Some FCP exports include `videoAngleID` and `audioAngleID` attributes, but these are **not declared in the v1.14 DTD**. Angle switching is handled through `<mc-source>` children instead.
*   **`<mc-source>`**: Used inside an `<mc-clip>` to define settings for a specific multicam angle.
    *   **`angleID`** (required): The ID of the target angle.
    *   **`srcEnable`**: `all` (default) | `audio` | `video` | `none`. Controls which streams from this angle are active.

### **Developer Example: Multicam Setup & Angle Switching**
*In this example, we have two cameras. The mc-clip uses `srcEnable` on `<mc-source>` to control which angle provides video vs. audio.*

```xml
<resources>
    <media id="r10" name="Interview Multicam">
        <multicam format="r1">
            <mc-angle name="Angle 1" angleID="cam1">
                <asset-clip ref="r2" offset="0s" duration="100s"/>
            </mc-angle>
            <mc-angle name="Angle 2" angleID="cam2">
                <asset-clip ref="r3" offset="0s" duration="100s"/>
            </mc-angle>
        </multicam>
    </media>
</resources>

<!-- In the timeline, mc-clip references the multicam media -->
<mc-clip ref="r10" offset="3600s" duration="10s" srcEnable="all">
    <!-- Switch to cam2 for video only on this angle -->
    <mc-source angleID="cam2" srcEnable="video"/>
</mc-clip>
```

### Synchronized Clips
A **Synchronized clip** (`<sync-clip>`) groups independently recorded media that should play in sync (e.g., a camera recording and a separate audio recorder). It uses `<sync-source>` children to map each source's timing.

*   **`<sync-clip>`**: Container for synchronized media.
    *   Standard clip attributes: `ref`, `offset`, `start`, `duration`, `format`.
    *   **`audioStart` / `audioDuration`**: Allow split edits (J/L cuts) where audio timing differs from video.
*   **`<sync-source>`**: Maps a source within the synchronized clip.
    *   **`sourceID`**: Identifies the source type. Enumeration: `storyline` (primary video) or `connected` (additional sources/audio).

### Auditions
An **Audition** (`<audition>`) is a container that lets editors try out alternative clips in the same timeline position. Only one clip is active (the "pick"); the rest are hidden alternatives.

*   **`<audition>`**: Container for audition alternatives.
    *   The **first child** element is the active "pick" shown in the timeline.
    *   Subsequent children are hidden alternatives the editor can swap in.
    *   Can contain: `<audio>`, `<video>`, `<title>`, `<ref-clip>`, `<asset-clip>`, `<clip>`, `<sync-clip>`, `<live-drawing>`.

### Live Drawings
A **Live Drawing** (`<live-drawing>`) is a vector graphics animation element (introduced in v1.11). It can appear in spines, as connected clips, or inside auditions. Live drawings reference external PKDrawing data files via a `<locator>` resource. See Section 16.1 for a developer example.

### Connected Storylines
A `<spine>` element can appear not only as the primary storyline inside a `<sequence>`, but also as a **child of a clip**. When nested this way, it forms a "connected storyline" — a secondary storyline that is attached to and moves with the parent clip. Connected clips use the **`lane`** attribute on their parent element to position vertically relative to the primary storyline.

---

## 4. Metadata, Markers (Locators) & Captions
These elements provide context and organization. In FCPXML, "locators" are implemented via various marker tags.

*   **`<marker>`**: A point-of-interest locator. Use it for general notes.
*   **`<chapter-marker>`**: A locator that defines a "jump point" for DVD/Blu-ray or video players (YouTube chapters).
*   **`<analysis-marker>`**: Created automatically by FCP for things like "Face Detection" or "Shaky Video."
*   **`<caption>`**: A closed caption element (introduced in FCPXML v1.8).
    *   Contains `<text>`, `<text-style-def>`, and optional `<note>`.
    *   **`role`**: Caption role assignment. For standard captions, this follows the format `[Role]?captionFormat=[Format].[Language]` (e.g., `SRT?captionFormat=SRT.en`).
    *   Has standard clip attributes (`offset`, `duration`, etc.).
*   **`<note>`**: A simple text element for attaching notes to clips or other elements.

### **The Roles System**
Roles are a hierarchical labeling system that organizes clips by function (e.g., dialogue, music, effects, titles). They use a `role.subrole` dot-separated format (e.g., `dialogue.dialogue-1`). The `role` attribute appears on many elements including `<asset-clip>`, `<clip>`, `<audio>`, `<video>`, `<caption>`, `<title>`, `<audio-channel-source>`, and `<audio-role-source>`. Roles control audio lane organization in the timeline and are used during export to create separate audio stems.

### **Developer Example: Locators & Metadata**
```xml
<asset-clip ref="r2" offset="0s" duration="10s">
    <!-- A standard marker (locator) -->
    <marker start="2s" value="Good take starts here"/>

    <!-- A chapter marker with a specific thumbnail frame -->
    <chapter-marker start="5s" duration="1/24s" value="Chapter 1" posterOffset="0s"/>

    <metadata>
        <md key="com.apple.proapps.studio.notes" value="Check audio levels at 2s"/>
    </metadata>
</asset-clip>
```


---

## 5. Visual & Audio Adjustments
These define how a clip looks and sounds. They are always interpreted within the context of the **referenced format**.

### **The Coordinate System (Center-Based)**
Unlike most graphics engines where `(0,0)` is the top-left, FCPXML uses the **center of the format** as the origin `(0,0)`.
*   **X-axis**: Positive to the right, negative to the left.
*   **Y-axis**: Positive upwards, negative downwards.
*   **Example**: In a 1920x1080 format, a position of `-960 540` would place the clip's anchor point at the top-left corner of the screen.

### **Video Adjustment Tags**
*   **`<adjust-transform>`**: 
    *   **`enabled`**: `1` (default) or `0`. Enables/disables the adjustment.
    *   **`position`**: Translation from the center, expressed as **percentages of the frame height**. (e.g., `10 10` is 10% right and 10% up).
    *   **`scale`**: Multiplier (e.g., `1 1` is 100%, `2 2` is 200%).
    *   **`rotation`**: Spinning the clip in degrees. **Positive is counter-clockwise**, negative is clockwise.
        > **Rotation convention note:** UltimateSlice stores rotation using this same convention (positive = counter-clockwise), matching FCP. When exporting via FFmpeg, the stored angle must be **negated** because FFmpeg's `rotate` filter treats positive angles as clockwise (screen-coordinate Y-down convention). GStreamer's `GstRotate` element uses counter-clockwise-positive and needs no negation; the `videoflip` fallback (90°-increment only) must map `+90` → `counterclockwise` and `+270`/`-90` → `clockwise`.
    *   **`anchor`**: The point on the clip that "sticks" to the position, expressed as **percentages of the frame height** (relative to the clip's center).
    *   **`tracking`**: An IDREF linking to an object tracker for automated motion tracking integration.
*   **`<adjust-conform>`**: 
    *   Determines how to fit a clip into a sequence when their formats (resolutions) don't match.
    *   **`type="fit"`**: Scales the clip to fit entirely inside the frame (letterboxing if needed).
    *   **`type="fill"`**: Scales the clip to fill the entire frame (cropping if needed).
    *   **`type="none"`**: Keeps the clip at its original pixel size.
*   **`<adjust-crop>`**: Trims the edges of the video.
*   **`<adjust-blend>`**: Controls opacity and compositing blend mode.
    *   **`amount`**: Opacity from `0.0` (transparent) to `1.0` (opaque). Default: `1.0`.
    *   **`mode`**: Blend mode as an **integer value**. If omitted, uses normal compositing (0). Common values: `0` (Normal), `2` (Subtract), `4` (Multiply), `10` (Screen), `14` (Overlay). See the Blend Mode Reference table below for the full list.

### **Audio Attributes**
*   **`amount` (Volume)**: Measured in decibels (dB). `0dB` is the original volume. `-inf` is muted.
*   **`fadeIn/fadeOut`**: Automatically smooths the start or end of the sound.

### **Audio Channel Configuration**
*   **`<audio-channel-source>`**: Maps source audio channels to output channels.
    *   **`srcCh`**: Source channel(s) (e.g., `"1, 2"`).
    *   **`outCh`**: Output channel assignment (e.g., `"L, R"`).
    *   **`role`**: Audio role for these channels.
    *   **`enabled`**: Whether the channel source is active (`1` or `0`).
    *   **`active`**: Whether the channel is actively playing (`1` or `0`).
*   **`<audio-role-source>`**: Manages role-based audio mixing.
    *   **`role`**: The audio role (e.g., `dialogue.dialogue-1`).
    *   **`enabled`**: Whether this role source is active (`1` or `0`).
    *   **`active`**: Whether this role is actively playing (`1` or `0`).

### **Timing & Keyframes**
Keyframes define how a parameter (like Opacity, Position, or an Effect value) changes over time. In FCPXML, these are organized under the `<keyframeAnimation>` container, usually as a child of a `<param>` element.

#### **The Animation Structure**
*   **`<keyframeAnimation>`**: The container for a sequence of keyframes.
*   **`<keyframe>`**: A specific point in time with a value and interpolation rules.
    *   **`time`**: Rational number (e.g., `100/2400s`). This time is **relative to the start of the element** (the clip), not the absolute timeline.
    *   **`value`**: The parameter value at that specific time.
    *   **`interp`**: Temporal interpolation for the segment *following* this keyframe.
        *   `linear` (default): Constant rate of change.
        *   `ease`: Smooth acceleration and deceleration.
        *   `easeIn`: Smooth acceleration (slow start, fast end).
        *   `easeOut`: Smooth deceleration (fast start, slow end).
    *   **`curve`**: High-level velocity profile preset: `linear` or `smooth` (default: `smooth`).

#### **Temporal vs. Spatial Interpolation**
FCP distinguishes between how a value changes *over time* (Temporal) and how a point moves *through space* (Spatial).

**1. Temporal Keyframes (Scalar)**
Used for single-value parameters like Opacity, Scale, or Volume.
```xml
<param name="Opacity" value="100">
    <keyframeAnimation>
        <!-- Fade in over 2 seconds -->
        <keyframe time="0s" value="0" interp="easeIn"/>
        <keyframe time="2s" value="100"/>
    </keyframeAnimation>
</param>
```

**2. Point-Value Keyframes (Position & Anchor)**
Used for multi-dimensional parameters like `position` and `anchor`. These use the same `<keyframe>` element, but the `value` attribute contains space-separated coordinates.
*   **Value Format**: `value="x y"` (expressed in height-percentages relative to format center).
*   **Interpolation**: Uses the same `interp` and `curve` attributes as scalar keyframes. The `curve="smooth"` preset provides natural-looking motion between points.

```xml
<param name="position" value="0 0">
    <keyframeAnimation>
        <!-- Linear movement from left to right -->
        <keyframe time="0s" value="-50 0" interp="linear"/>
        <keyframe time="5s" value="50 0"/>
    </keyframeAnimation>
</param>
```

```xml
<param name="position" value="0 0">
    <keyframeAnimation>
        <!-- Smooth eased movement -->
        <keyframe time="0s" value="-50 -25" interp="ease" curve="smooth"/>
        <keyframe time="3s" value="0 25" interp="ease" curve="smooth"/>
        <keyframe time="5s" value="50 -25"/>
    </keyframeAnimation>
</param>
```

#### **Transformation Math (Developer Guide)**

> **Note:** The unit system for `position` and `anchor` is not explicitly defined as pixels in the DTD. The **percentage of frame height** interpretation below is the industry standard (e.g., as used by tools like FCP.cafe and various translation layers) to ensure scaling remains proportional if a timeline's resolution is changed.

When building a renderer, use these formulas to convert FCPXML values into standard graphics engine coordinates. The following assumes **normalized coordinates based on frame height**.

#### **1. Proportional-to-Pixel Conversion**
FCPXML units for `position` and `anchor` are percentages of the **Frame Height**.
*   **Formula**: `PixelValue = (FCPXMLValue / 100) * FrameHeight`
*   **Example**: In a `1920x1080` format, a Y-position of `50`:
    *   `py_pixels = (50 / 100) * 1080 = 540 pixels` (reaches the top edge).

#### **2. Center-to-TopLeft Conversion**
To convert FCPXML `(x, y)` to standard top-left `(px, py)` pixel coordinates:
*   **Step 1**: Convert XML units to pixels (using Frame Height for both X and Y).
    *   `x_px = (x / 100) * FrameHeight`
    *   `y_px = (y / 100) * FrameHeight`
*   **Step 2**: Offset from the center.
    *   `px = (FormatWidth / 2) + x_px`
    *   `py = (FormatHeight / 2) - y_px`
*   **Example**: In a `1920x1080` format, a position of `10 10`:
    *   `x_px = (10 / 100) * 1080 = 108`
    *   `y_px = (10 / 100) * 1080 = 108`
    *   `px = 960 + 108 = 1068`
    *   `py = 540 - 108 = 432`

#### **3. Applying the Anchor Point**
The `anchor` attribute shifts the clip relative to its own center (also in height-percentage units).
*   **Formula**: `FinalPosition = Position - Anchor`

#### **4. Conform Scaling (Fit vs. Fill)**
When a source asset (e.g., 4K) doesn't match the timeline (e.g., 1080p), calculate the scale factor:
*   **Setup**: 
    *   Timeline: `1920x1080` (AR 1.77)
    *   Asset: `4000x3000` (AR 1.33)
*   **Calculations**:
    *   `scaleX = 1920 / 4000 = 0.48`
    *   `scaleY = 1080 / 3000 = 0.36`
*   **Results**:
    *   **Type="fit"**: Use `min(scaleX, scaleY) = 0.36`. The clip will be 1440x1080 (letterboxed).
    *   **Type="fill"**: Use `max(scaleX, scaleY) = 0.48`. The clip will be 1920x1440 (cropped).

### **Developer Example: Animated Opacity Fade (Blend)**
*This clip starts invisible and takes 2 seconds to fade in to full 100% visibility. Per the DTD, opacity is controlled by `<adjust-blend>`, not `<adjust-transform>`.*

```xml
<adjust-blend amount="0.0">
    <param name="amount">
        <keyframeAnimation>
            <keyframe time="0s" value="0.0"/>
            <keyframe time="2s" value="1.0"/>
        </keyframeAnimation>
    </param>
</adjust-blend>
```

### **Blend Mode Reference**
The `mode` attribute on `<adjust-blend>` takes an integer value. Gaps in the numbering correspond to category separators in the FCP UI.

| Integer | Blend Mode | Integer | Blend Mode |
|---------|------------|---------|------------|
| `0` | Normal | `17` | Vivid Light |
| `2` | Subtract | `18` | Linear Light |
| `3` | Darken | `19` | Pin Light |
| `4` | Multiply | `20` | Hard Mix |
| `5` | Color Burn | `22` | Difference |
| `6` | Linear Burn | `23` | Exclusion |
| `8` | Add | `25` | Stencil Alpha |
| `9` | Lighten | `26` | Stencil Luma |
| `10` | Screen | `27` | Silhouette Alpha |
| `11` | Color Dodge | `28` | Silhouette Luma |
| `12` | Linear Dodge | `29` | Behind |
| `14` | Overlay | `31` | Alpha Add |
| `15` | Soft Light | `32` | Premultiplied Mix |
| `16` | Hard Light | | |

### **Developer Example: Blend Modes**
```xml
<!-- Multiply blend at 75% opacity -->
<adjust-blend amount="0.75" mode="4"/>

<!-- Screen blend at full opacity -->
<adjust-blend amount="1.0" mode="10"/>
```

---

## 6. Understanding Time (The Most Important Part)
FCPXML does not use decimals like `2.5 seconds`. It uses **Rational Numbers** (Fractions) like `250/100s`.

**Why?** Decimals like `1/3` (0.333...) lead to "rounding errors" over time. In a 2-hour movie, a tiny rounding error will cause the audio to drift out of sync with the video. Fractions are mathematically perfect and never drift.

### **The Timing Formula**
To build an editor, you must calculate where a frame from a source file appears on the timeline:
1.  **`TimelineTime`**: Where the playhead is right now.
2.  **`Offset`**: Where the clip starts on the timeline.
3.  **`Start`**: What point in the original file we began cutting from.

**Formula:** `SourcePosition = (CurrentPlayhead - Offset) + Start`

### **Speed & Retime Effects**
Variable speed (retime) effects are controlled by `<timeMap>` and `<timept>` elements. These map output (timeline) time to source (media) time, allowing speed ramps, freeze frames, and reverse playback.

*   **`<timeMap>`**: Container for speed mapping points.
    *   **`frameSampling`**: Quality mode for interpolated frames: `floor` | `nearest-neighbor` | `frame-blending` | `optical-flow-classic` | `optical-flow` | `optical-flow-frc`. Default: `floor`.
    *   **`preservesPitch`**: Whether to preserve audio pitch during speed changes (`0` | `1`). Default: `1`.
*   **`<timept>`**: A single time mapping point within a `<timeMap>`.
    *   **`time`**: The output (timeline) time.
    *   **`value`**: The corresponding source (media) time.
    *   **`interp`**: Interpolation curve between points: `smooth2` | `linear` | `smooth`. Default: `smooth2`.

#### **Developer Example: 2x Speed Ramp**
*Maps 10 seconds of timeline to 20 seconds of source media, producing 2x playback speed.*

```xml
<timeMap>
    <timept time="0s" value="0s" interp="smooth2"/>
    <timept time="10s" value="20s" interp="smooth2"/>
</timeMap>
```

*   **`<conform-rate>`**: Part of timing parameters. Controls frame rate conforming when a clip's native frame rate differs from the sequence.
    *   **`scaleEnabled`**: Whether rate conforming is active (`0` | `1`). Default: `1`.
    *   **`srcFrameRate`**: The source frame rate to conform from. Enumeration: `23.98` | `24` | `25` | `29.97` | `30` | `60` | `47.95` | `48` | `50` | `59.94` | `90` | `100` | `119.88` | `120`.
    *   **`frameSampling`**: Frame interpolation method: `floor` (default) | `nearest-neighbor` | `frame-blending` | `optical-flow-classic` | `optical-flow` | `optical-flow-frc`.

---

## 7. Implementation Checklist for an Editor
If you are writing code to parse FCPXML, follow these steps:

1.  **The Resource Dictionary**: Read the `<resources>` section first. Store every `format` and `asset` in a Map/Dictionary. When you see `ref="r1"`, look up `r1` in your map.
2.  **The Fraction Class**: Write or find a library to handle fractions (e.g., `30000/1001`). Do all your math with these objects.
3.  **The Recursive Renderer**: 
    *   Look at the `<spine>`. 
    *   For every clip, check its `lane`. 
    *   Render `lane 0` (the main story) first.
    *   Then "layer" higher lanes on top based on their `position`, `scale`, and `opacity`.
---

## 8. Comprehensive Tag & Attribute Reference

### **Root & Organizational Tags**
*   **`<fcpxml>`**: The root element.
    *   **`version`**: The schema version (e.g., `1.10`, `1.11`, `1.12`, `1.13`, `1.14`).
*   **`<library>`**: Represents an FCP library.
    *   **`location`**: The file URL to the library on disk.
    *   **`colorProcessing`**: `standard` | `wide` | `wide-hdr`. See Section 13.
*   **`<event>`**: A container for clips and projects.
    *   **`name`**: The display name of the event.
    *   **`uid`**: FCP-assigned unique identifier.
*   **`<project>`**: Represents a Final Cut Pro project (timeline).
    *   **`name`**: The display name of the project.
    *   **`uid`**: FCP-assigned unique identifier.
    *   **`id`**: XML ID for internal referencing.
    *   **`modDate`**: Modification date.

### **Resource Tags (Inside `<resources>`)**
*   **`<format>`**: Defines resolution and frame rate.
    *   **`id`**: Unique resource ID.
    *   **`name`**: Symbolic name (e.g., `FFVideoFormat1080p24`).
    *   **`frameDuration`**: Frame time in rational seconds (e.g., `100/2400s`).
    *   **`width` / `height`**: Frame dimensions in pixels.
    *   **`colorSpace`**: The color profile (e.g., `Rec. 709`, `Rec. 2020`).
    *   **`fieldOrder`**: `progressive` | `upper first` | `lower first`.
    *   **`paspH` / `paspV`**: Pixel aspect ratio (horizontal/vertical).
    *   **`projection`**: `none` | `equirectangular` | `fisheye` | `back-to-back fisheye` | `cubic`.
    *   **`stereoscopic`**: `mono` | `side by side` | `over under`.
    *   **`heroEye`**: `left` | `right`.
*   **`<asset>`**: References a physical media file. Contains `<media-rep>+` and optional `<metadata>`.
    *   **`id`**: Unique resource ID.
    *   **`name`**: Display name.
    *   **`uid`**: A unique ID used for media linking.
    *   **`start` / `duration`**: Intrinsic bounds of the file.
    *   **`hasVideo` / `hasAudio`**: Boolean flags (`1` or `0`).
    *   **`format`**: Reference to a `<format>` resource ID.
    *   **`videoSources`**: Number of video sources.
    *   **`audioSources`** / **`audioChannels`** / **`audioRate`**: Audio stream properties.
    *   **`customLUTOverride`**: Custom LUT or built-in log mode identifier.
    *   **`colorSpaceOverride`**: Override the auto-detected color space.
    *   **`projectionOverride`**: Override projection type.
    *   **`stereoscopicOverride`**: Override stereoscopic mode.
    *   **`heroEyeOverride`**: Override hero eye (`left` | `right`).
    *   **`auxVideoFlags`**: Auxiliary video flags.
*   **`<media-rep>`**: Physical file reference within an `<asset>`.
    *   **`src`** (required): File URL.
    *   **`kind`**: `original-media` (default) | `proxy-media`.
    *   **`sig`**: File signature (assigned by FCP).
    *   **`suggestedFilename`**: Filename hint with extension.
    *   Can contain optional `<bookmark>` child.
*   **`<media>`**: Represents synthetic or nested media (Compound/Multicam).
    *   **`id`**: Unique resource ID.
    *   **`name`**: Display name.
    *   **`uid`**: FCP-assigned unique identifier.
    *   **`projectRef`**: IDREF to a project.
    *   **`modDate`**: Modification date.
*   **`<effect>`**: Defines a plugin or filter.
    *   **`id`**: Unique resource ID.
    *   **`uid`** (required): The system path/ID for the effect plugin.
    *   **`name`**: Display name.
    *   **`src`**: Source path for custom effects.
*   **`<locator>`**: URL reference resource. Used for external data files (e.g., tracking data, drawing data).
    *   **`id`** (required): Unique resource ID.
    *   **`url`** (required): The URL of the referenced resource.
    *   Can contain optional `<bookmark>` child.

### **Timeline & Story Elements**
*   **`<sequence>`**: The main timeline container.
    *   **`format`** (required): Reference to a `<format>` ID.
    *   **`duration`**: Total length of the sequence.
    *   **`tcStart`**: Starting timecode (usually `3600s`).
    *   **`tcFormat`**: Timecode format (`DF` or `NDF`).
    *   **`audioLayout`**: `mono` | `stereo` | `surround`.
    *   **`audioRate`**: Audio sample rate (`32k` | `44.1k` | `48k` | `88.2k` | `96k` | `176.4k` | `192k`).
    *   **`renderFormat`**: Codec used for preview render files.
    *   **`keywords`**: Keywords associated with the sequence.
*   **`<spine>`**: The primary "storyline" of the timeline.
    *   **`name`**: Display name of the spine.
    *   **`format`**: Reference to a `<format>` ID (defaults to parent's format).
*   **`<asset-clip>`**: A clip referencing an `<asset>`.
    *   **`ref`** (required): Reference to an `<asset>` ID.
    *   **`offset`**: Start time relative to parent.
    *   **`start` / `duration`**: Range within the asset. Duration is optional (defaults to asset's full duration).
    *   **`lane`**: Vertical position (default `0`).
    *   **`audioRole`**: Audio role assignment.
    *   **`videoRole`**: Video role assignment (default: `video`).
    *   **`srcEnable`**: `all` (default) | `audio` | `video`. Controls which streams are active.
    *   **`format`**: Reference to a `<format>` ID (defaults to parent's format).
    *   **`audioStart` / `audioDuration`**: For split edits (J/L cuts).
    *   **`tcStart` / `tcFormat`**: Clip timecode origin and format.
    *   **`modDate`**: Modification date.
*   **`<clip>`**: A generic clip element (distinct from `<asset-clip>`).
    *   Supports `format`, `audioStart` / `audioDuration` (for split edits), `tcStart` / `tcFormat`.
    *   **`modDate`**: Modification date.
    *   Can contain nested spines, captions, markers, and filters.
*   **`<video>`**: A video-only clip referencing an asset or an effect (generator).
    *   **`ref`**: Reference to an `<asset>` ID or `<effect>` ID (for generators — see Section 11).
    *   **`srcID`**: Source identifier within the asset.
    *   **`role`**: Video role assignment.
    *   Can contain params, filters, and markers.
*   **`<audio>`**: An audio-only clip referencing an asset.
    *   **`ref`**: Reference to an `<asset>` ID.
    *   **`srcID`**: Source identifier within the asset.
    *   **`role`**: Audio role assignment.
    *   **`srcCh`**: Source audio channels (e.g., `"1, 2"`).
    *   **`outCh`**: Output channel assignment (e.g., `"L, R"`).
    *   Can contain `<adjust-volume>`, filters, and markers.
*   **`<sync-clip>`**: A synchronized clip grouping independently recorded media.
    *   **`format`**: Reference to a `<format>` ID.
    *   **`audioStart` / `audioDuration`**: For split edits.
    *   **`tcStart` / `tcFormat`**: Clip timecode origin and format.
    *   **`modDate`**: Modification date.
    *   Contains `<sync-source>` children mapping source streams.
*   **`<audition>`**: Container for audition alternatives.
    *   First child is the active "pick"; subsequent children are alternatives.
    *   **`modDate`**: Modification date.
    *   Can contain: `<audio>`, `<video>`, `<title>`, `<ref-clip>`, `<asset-clip>`, `<clip>`, `<sync-clip>`, `<live-drawing>`.
*   **`<mc-clip>`**: A clip referencing a `<multicam>` media resource.
    *   **`ref`** (required): Reference to a `<media>` ID.
    *   **`srcEnable`**: `all` (default) | `audio` | `video`. Controls which streams are active.
    *   **`audioStart` / `audioDuration`**: For split edits.
    *   **`modDate`**: Modification date.
    > **Note:** Some FCP exports include `videoAngleID`/`audioAngleID` attributes not declared in the v1.14 DTD.
*   **`<ref-clip>`**: A clip referencing a `<media>` resource (Compound clip).
    *   **`ref`** (required): Reference to a `<media>` ID.
    *   **`srcEnable`**: `all` (default) | `audio` | `video`.
    *   **`audioStart` / `audioDuration`**: For split edits.
    *   **`useAudioSubroles`**: `0` (default) | `1`.
    *   **`modDate`**: Modification date.
*   **`<gap>`**: A placeholder for empty space.
    *   **`duration` / `offset`**: Position and length.
*   **`<transition>`**: A transition effect between two clips.
    *   **`name`**: Display name of the transition.
    *   **`offset`**: Position on the timeline.
    *   **`duration`**: Length of the transition.
    *   Contains optional `<filter-video>`, `<filter-audio>`, markers, and metadata.
*   **`<title>`**: A text overlay generator referencing an `<effect>` resource.
    *   **`ref`**: Reference to an `<effect>` ID (e.g., Basic Title, Custom Title).
    *   **`role`**: Typically `titles.titles-1`.
    *   Standard clip attributes (`offset`, `duration`, `lane`, `enabled`).
    *   Contains `<param>*`, `<text>*`, `<text-style-def>*`, `<note>?`, adjustments, filters, markers.
    *   See Section 11 for full title/text documentation.
*   **`<caption>`**: A closed caption element.
    *   **`role`**: Caption role assignment.
    *   Standard clip attributes (`offset`, `duration`, etc.).
    *   Contains `<text>`, `<text-style-def>`.

### **Metadata & Locators**
*   **`<marker>`**: A point-of-interest locator.
    *   **`start`** (required): Position relative to the clip's start.
    *   **`duration`**: Length of the marker range.
    *   **`value`** (required): Note or name.
    *   **`completed`**: When present, turns the marker into a to-do item. `0` = not completed, `1` = completed.
    *   **`note`**: Additional notes for the marker.
*   **`<chapter-marker>`**: A locator for chapter markers.
    *   **`start`** (required): Position relative to the clip's start.
    *   **`duration`**: Length of the chapter range.
    *   **`value`** (required): Chapter name.
    *   **`note`**: Additional notes.
    *   **`posterOffset`**: Thumbnail frame position.
*   **`<rating>`**: A favorite/reject marker applied to a time range.
    *   **`value`** (required): `favorite` | `reject`.
    *   **`start` / `duration`**: The range covered.
    *   **`name`**: Display name.
    *   **`note`**: Additional notes.
*   **`<keyword>`**: A tag applied to a time range.
    *   **`value`** (required): Comma-separated list of keywords.
    *   **`start` / `duration`**: The range covered by the tag.
    *   **`note`**: Additional notes.
*   **`<analysis-marker>`**: Created automatically by FCP for analysis results (face detection, shake detection).
    *   **`start` / `duration`**: The analyzed range.
    *   Children: one or more `<shot-type>` or `<stabilization-type>` elements.
*   **`<shot-type>`**: Analysis result for shot composition.
    *   **`value`** (required): `onePerson` | `twoPersons` | `group` | `closeUp` | `mediumShot` | `wideShot`.
*   **`<stabilization-type>`**: Analysis result for camera stability.
    *   **`value`** (required): `excessiveShake`.
*   **`<hidden-clip-marker>`**: An internal empty marker element.
*   **`<metadata>`**: Container for key-value metadata.
    *   **`<md>`**: A metadata entry.
        *   **`key`** (required): Metadata key identifier.
        *   **`value`**: Metadata value.
        *   **`editable`**: `0` (default) | `1`.
        *   **`type`**: `string` | `boolean` | `integer` | `float` | `date` | `timecode`.
        *   **`displayName`**: Human-readable name.
        *   **`description`**: Description of the metadata field.
        *   **`source`**: Source identifier.
        *   Can contain an `<array>` child with `<string>` elements for multi-value metadata.
*   **`<note>`**: A simple text element (PCDATA) for attaching notes to clips or other elements.

### **Adjustments & Effects**
*   **`<adjust-transform>`**: Spatial transformations.
    *   **`position`**: XY translation (percentage of height).
    *   **`scale`**: XY multiplier.
    *   **`rotation`**: Rotation in degrees (positive is CCW).
    *   **`anchor`**: Center of transformation (percentage of height).
    *   **`tracking`**: IDREF linking to an object tracker.
*   **`<adjust-blend>`**: Opacity and compositing blend mode.
    *   **`amount`**: Opacity from `0.0` to `1.0` (default: `1.0`).
    *   **`mode`**: Blend mode as an integer (e.g., `2` for Subtract, `4` for Multiply). Omit for normal compositing. See the Blend Mode Reference table in Section 5.
*   **`<adjust-conform>`**: Resolution scaling rules.
    *   **`type`**: `fit`, `fill`, or `none`.
*   **`<adjust-crop>`**: Trims the edges of the video.
    *   **`mode`**: Crop mode (`trim`, `crop`, `pan`). See Section 14 for detailed mode documentation.
    *   **`enabled`**: `0` or `1` (default: `1`).
*   **`<adjust-volume>`**: Audio level controls.
    *   **`amount`**: Volume in dB.
*   **`<filter-video>`**: A video effect applied to its parent element.
    *   **`ref`** (required): Reference to an `<effect>` ID.
    *   **`name`**: Display name.
    *   **`nameOverride`**: Overrides the display name.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   Can contain `<data>` and `<param>` children.
*   **`<filter-audio>`**: An audio effect applied to its parent element.
    *   **`ref`** (required): Reference to an `<effect>` ID.
    *   **`name`**: Display name.
    *   **`nameOverride`**: Overrides the display name.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`presetID`**: Preset identifier.
    *   Can contain `<data>` and `<param>` children.

### **Audio Channel Configuration**
*   **`<audio-channel-source>`**: Maps source audio channels to output channels.
    *   **`srcCh`** (required): Source channel(s) (e.g., `"1, 2"`).
    *   **`outCh`**: Output channel assignment (e.g., `"L, R"`).
    *   **`role`**: Audio role for these channels.
    *   **`start` / `duration`**: Time range within the clip.
    *   **`enabled`** / **`active`**: Boolean flags (`1` or `0`). Both default to `1`.
    *   Contains audio enhancement adjustments, audio intrinsic params, `<filter-audio>*`, and `<mute>*`.
*   **`<audio-role-source>`**: Manages role-based audio mixing.
    *   **`role`** (required): The audio role (e.g., `dialogue.dialogue-1`).
    *   **`start` / `duration`**: Time range within the clip.
    *   **`enabled`** / **`active`**: Boolean flags (`1` or `0`). Both default to `1`.
    *   Contains audio enhancement adjustments, audio intrinsic params, `<filter-audio>*`, and `<mute>*`.
*   **`<mute>`**: Suppresses audio output for a range of source media time.
    *   **`start` / `duration`**: The muted time range.
    *   Can contain optional `<fadeIn>` and `<fadeOut>` children for smooth transitions.

### **Speed & Retime**
*   **`<timeMap>`**: Container for speed mapping points.
    *   **`frameSampling`**: `floor` | `nearest-neighbor` | `frame-blending` | `optical-flow-classic` | `optical-flow` | `optical-flow-frc`.
    *   **`preservesPitch`**: `0` | `1` (default: `1`).
*   **`<timept>`**: A time mapping point.
    *   **`time`**: Output (timeline) time.
    *   **`value`**: Corresponding source time.
    *   **`interp`**: `smooth2` | `linear` | `smooth` (default: `smooth2`).
*   **`<conform-rate>`**: Frame rate conforming.
    *   **`scaleEnabled`**: `0` | `1` (default: `1`).
    *   **`srcFrameRate`**: `23.98` | `24` | `25` | `29.97` | `30` | `60` | `47.95` | `48` | `50` | `59.94` | `90` | `100` | `119.88` | `120`.
    *   **`frameSampling`**: `floor` (default) | `nearest-neighbor` | `frame-blending` | `optical-flow-classic` | `optical-flow` | `optical-flow-frc`.

---

## 9. Comprehensive FCPXML Example
This complete example demonstrates how resources, timelines, multicam clips, keyframes, and roles interact in a production-ready file.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.14">
    <resources>
        <!-- 1. Format: Defines 1080p 24fps canvas -->
        <format id="r1" name="FFVideoFormat1080p24" frameDuration="100/2400s" width="1920" height="1080" colorSpace="1-1-1 (Rec. 709)"/>

        <!-- 2. Assets: Physical media files (src is on media-rep, not on asset) -->
        <asset id="r2" name="Interview_CamA"
               start="0s" duration="3600s" hasVideo="1" hasAudio="1">
            <media-rep kind="original-media" src="file:///media/camera_a.mov"/>
        </asset>
        <asset id="r3" name="Interview_CamB"
               start="0s" duration="3600s" hasVideo="1" hasAudio="1">
            <media-rep kind="original-media" src="file:///media/camera_b.mov"/>
        </asset>
        <asset id="r4" name="External_Audio"
               start="0s" duration="3600s" hasVideo="0" hasAudio="1">
            <media-rep kind="original-media" src="file:///media/audio_rec.wav"/>
        </asset>
        
        <!-- 3. Multicam Resource: Groups Cam A and Cam B -->
        <media id="r5" name="Interview Multicam">
            <multicam format="r1">
                <mc-angle name="Camera A" angleID="cam1">
                    <asset-clip ref="r2" offset="0s" duration="3600s" audioRole="dialogue.dialogue-1"/>
                </mc-angle>
                <mc-angle name="Camera B" angleID="cam2">
                    <asset-clip ref="r3" offset="0s" duration="3600s" audioRole="dialogue.dialogue-2"/>
                </mc-angle>
            </multicam>
        </media>
        
        <!-- 4. Effects: Referenced by filters later -->
        <effect id="r8" name="Color Board" uid=".../Video Filters/Color/Color Board"/>
    </resources>
    
    <library location="file:///Users/dev/Movies/MyProject.fcpproject/">
        <event name="Main Event">
            <project name="Final Export Timeline">
                <sequence format="r1" tcStart="3600s" tcFormat="NDF">
                    <spine>
                        <!-- 5. Asset Clip with Animation (Keyframes) -->
                        <asset-clip ref="r2" offset="3600s" name="Intro Shot" start="10s" duration="5s">
                            <!-- Position Keyframes: Smooth eased movement across the frame -->
                            <adjust-transform>
                                <param name="position" value="0 0">
                                    <keyframeAnimation>
                                        <keyframe time="0s" value="-50 0" interp="ease" curve="smooth"/>
                                        <keyframe time="5s" value="50 0"/>
                                    </keyframeAnimation>
                                </param>
                            </adjust-transform>
                            
                            <!-- Temporal Keyframes: Simple Opacity Fade -->
                            <adjust-blend amount="1.0">
                                <param name="amount">
                                    <keyframeAnimation>
                                        <keyframe time="4s" value="1.0" interp="easeIn"/>
                                        <keyframe time="5s" value="0.0"/>
                                    </keyframeAnimation>
                                </param>
                            </adjust-blend>
                            
                            <!-- Markers & Metadata -->
                            <marker start="2s" value="Great expression here"/>
                            <chapter-marker start="0s" value="Start of Film" posterOffset="0s"/>
                            
                            <!-- 6. Connected Storyline: Attached to the Intro Shot -->
                            <spine lane="1" offset="2s">
                                <asset-clip ref="r3" offset="0s" duration="2s" role="video.b-roll"/>
                            </spine>
                        </asset-clip>
                        
                        <!-- 7. Multicam Clip: Switching angles mid-segment -->
                        <mc-clip ref="r5" offset="3605s" duration="10s" srcEnable="all">
                            <mc-source angleID="cam2" srcEnable="video"/>
                        </mc-clip>
                        
                        <!-- 8. Synchronized Clip: Combining Camera Video with External Audio -->
                        <sync-clip format="r1" offset="3615s" duration="5s">
                            <sync-source sourceID="storyline">
                                <asset-clip ref="r2" offset="0s" duration="5s"/>
                            </sync-source>
                            <sync-source sourceID="connected">
                                <asset-clip ref="r4" offset="0s" duration="5s" role="dialogue.external"/>
                            </sync-source>
                        </sync-clip>
                        
                        <!-- 9. Audition: Trying out two different shots -->
                        <audition offset="3620s">
                            <asset-clip ref="r2" duration="2s" name="Option 1 (Pick)"/>
                            <asset-clip ref="r3" duration="2s" name="Option 2"/>
                        </audition>
                    </spine>
                </sequence>
            </project>
        </event>
    </library>
</fcpxml>
```

---

## 10. Implementation Edge Cases & Gotchas

This section documents practical pitfalls, default behaviors, and edge cases that developers commonly encounter when building FCPXML parsers or generators.

### 10.1 Rational Time Arithmetic Edge Cases

#### `0s` vs `0/1s` vs `0/30000s`
All three represent zero, but parsers must handle both formats:
*   **Whole-number shorthand**: FCP reduces fractions to whole seconds when possible (e.g., `5s` instead of `5000/1000s`). Zero is written as `0s`.
*   **Explicit rational form**: `0/1s` or `0/30000s` are equivalent to `0s`. Your parser must accept both `N/Ds` and `Ns` forms.
*   **Gotcha**: Do not assume the denominator carries frame rate information. `0/30000s` is still just zero -- the denominator does not imply 29.97fps context. The frame rate comes from the `<format>` element's `frameDuration`.

**Parser pattern**: Parse time strings with a regex like `^(-?\d+)(?:/(\d+))?s$`. If the denominator group is absent, treat it as `1`.

#### Negative Time Values
Negative values are valid in FCPXML. The CommandPost time library includes a `unm()` (unary minus) function for negation. Negative times appear in practice for:
*   **`tcStart`**: Some timelines start before 00:00:00:00.
*   **Connected clip offsets**: A connected clip can be positioned before the start of its parent clip, resulting in a negative offset relative to the parent.

#### Frame Boundary Alignment
All time values in a spine **must** land on a frame boundary of the sequence's format. If `frameDuration="100/2400s"` (24fps), then every `offset`, `start`, and `duration` must be an exact multiple of `100/2400s`. FCP will reject imports with the error **"The item is not on an edit frame boundary"** if values are not frame-aligned. When performing arithmetic, always snap results to the nearest frame boundary.

#### tcStart Interaction with Offsets
The `tcStart` on a `<sequence>` defines the timeline's timecode origin. Clips in the primary spine use `offset` values in the same coordinate space:
*   If `tcStart="3600s"` (01:00:00:00), the first clip's offset is typically `3600s`, **not** `0s`.
*   **Formula**: `TimelineTimecode = offset - tcStart + displayTimecodeStart`
*   **Gotcha for multicam**: Multicam clips use `tcStart` instead of `offset` for their internal timeline origin, and the implicit `start` value within a multicam is `0`.

### 10.2 Lane Numbering & Rendering Order

#### How Lanes Work
The `lane` attribute positions elements vertically relative to their parent container:
*   **`lane` omitted or `0`**: The element is **contained within** its parent (i.e., part of the primary storyline or the spine it belongs to). Elements in the spine at lane 0 are the base layer.
*   **Positive lanes** (`1`, `2`, `3`, ...): Connected clips **above** the primary storyline. Higher numbers are rendered on top of lower numbers. Video connected clips default to positive lanes.
*   **Negative lanes** (`-1`, `-2`, `-3`, ...): Connected clips **below** the primary storyline. Audio-only connected clips typically use negative lanes. Negative lane numbers are also used internally for audio channel component mapping (e.g., `lane="-1"` for component 2, `lane="-2"` for component 3).

#### Z-Order (Rendering/Compositing Order)
*   **Within a spine**: Elements are rendered in document order (first child is "bottom," last child is "top" if they overlap temporally). However, in the primary storyline, clips do not overlap -- they are sequential.
*   **Connected clips**: Higher lane numbers render on top. A clip at `lane="2"` composites over `lane="1"`, which composites over `lane="0"` (the primary storyline).
*   **Same lane**: If two connected clips share the same lane and overlap in time, document order determines stacking (later in the XML = on top).

#### Connected Clips and Gaps
Connected clips attach to a specific point on their parent clip via the `offset` attribute. The offset is relative to the **parent clip's local timeline** (the parent's `start` value defines the origin):
*   If a parent clip in the spine is moved, all its connected clips move with it.
*   **Gaps interact with connected clips**: A `<gap>` in the primary storyline can have connected clips. If you delete a gap, its connected clips are orphaned and will be removed on reimport.
*   Connected clips cannot exist independently -- they must be children of a clip or gap in the primary storyline.

### 10.3 The `enabled` Attribute

#### Which Elements Support It
The `enabled` attribute (type: `0` | `1`, default: `1`) appears on:
*   **All clip types**: `<clip>`, `<asset-clip>`, `<ref-clip>`, `<sync-clip>`, `<mc-clip>`, `<video>`, `<audio>`, `<title>`, `<gap>`
*   **Filters/Effects**: `<filter-video>`, `<filter-audio>`, `<filter-video-mask>`
*   **Audio channel configuration**: `<audio-channel-source>`, `<audio-role-source>`

#### What `enabled="0"` Means
*   **On a clip**: The clip is **disabled** -- it is visually and audibly skipped during playback. It still occupies its timeline position (it is not removed from the edit), but produces no output. Think of it as "muted + hidden" while preserving the edit structure.
*   **On a filter** (`<filter-video>` or `<filter-audio>`): The effect is bypassed. The clip renders as if the effect does not exist, but the effect's parameters are preserved.
*   **On `<audio-channel-source>`**: That specific audio channel mapping is muted. For example, disabling `srcCh="1"` silences channel 1 while leaving other channels active.
*   **On `<filter-video-mask>`**: The mask shape and its associated filter are bypassed.

#### The `active` Attribute (Distinct from `enabled`)
`<audio-channel-source>` and `<audio-role-source>` also have an `active` attribute (default: `1`). While `enabled` controls whether the element participates in output, `active` controls whether it is actively processing. Both default to `1`.

### 10.4 Split Edits (J/L Cuts)

#### How `audioStart` and `audioDuration` Work
Split edits allow the audio and video portions of a clip to have different in/out points. The attributes appear on composite A/V clip types: `<clip>`, `<asset-clip>`, `<ref-clip>`, `<sync-clip>`, and `<mc-clip>`.

*   **When omitted** (the common case): Audio and video share the same `start` and `duration`. No split edit exists.
*   **When present**: `audioStart` and `audioDuration` define an independent time range for the audio portion. The video portion continues to use the clip's `start` and `duration`.

#### Concrete Examples

**L-Cut** (audio extends beyond the video cut point):
```xml
<!-- Video: 5s of footage. Audio: starts at the same point but extends 2s longer -->
<asset-clip ref="r2" offset="3600s" start="10s" duration="5s"
            audioStart="10s" audioDuration="7s"/>
```
The video plays from 10s-15s of the source, but audio plays from 10s-17s, overlapping into the next clip's video.

**J-Cut** (audio starts before the video):
```xml
<!-- Video: 5s of footage starting at 10s. Audio: starts 2s earlier -->
<asset-clip ref="r2" offset="3600s" start="10s" duration="5s"
            audioStart="8s" audioDuration="7s"/>
```
The audio begins at 8s of the source (2s before the video cut-in), creating the characteristic "hear before you see" effect.

**Key rules**:
*   `audioStart` and `audioDuration` must always appear together -- you cannot specify one without the other.
*   The audio range can extend before or after the video range, but the clip's `offset` and visual `duration` in the timeline remain unchanged.
*   The audio range defined by `audioStart`/`audioDuration` is in the **source media's** time coordinate space, just like `start`.

### 10.5 Transition Overlap Model

#### How Transitions Work in the Spine
A `<transition>` element sits **between** two adjacent clips in a `<spine>` and overlaps both of them. From the DTD documentation:

> A `transition` element defines an effect that overlaps two adjacent story elements.

```xml
<spine>
    <asset-clip ref="r2" offset="0s" duration="5s"/>
    <transition duration="2s"/>
    <asset-clip ref="r3" offset="3s" duration="5s"/>
</spine>
```

#### The Duration-Sharing Model
The transition's `duration` is **shared** between the two adjacent clips:
*   The **outgoing clip** (left) contributes media from its tail -- the transition overlaps the **last N seconds** of this clip.
*   The **incoming clip** (right) contributes media from its head -- the transition overlaps the **first N seconds** of this clip.
*   The full transition duration overlaps both clips. In the example above, the 2s transition overlaps the last 2s of clip A and the first 2s of clip B.

#### Impact on Timeline Offsets
*   The transition **does not add** to the total timeline duration. The clips must have sufficient media handles (extra source media beyond their visible in/out points) to provide frames for the overlap.
*   The **offset of the incoming clip** is pulled earlier by the transition duration. In the example above, clip B's offset is `3s` (not `5s`), because the 2s transition causes 2s of overlap.
*   **Formula**: `IncomingClipOffset = OutgoingClipOffset + OutgoingClipDuration - TransitionDuration`
*   The transition's own `offset` attribute, when present, matches the point where the outgoing clip ends minus the transition duration (i.e., where the overlap begins).

#### Edge Cases
*   Transitions can only exist inside a `<spine>` -- they cannot be applied to connected clips directly.
*   A transition between a clip and a `<gap>` is valid (it fades to/from black).
*   The transition element can contain optional `<filter-video>` and `<filter-audio>` children that define the actual effect (e.g., cross dissolve, wipe).

### 10.6 Audio Panning

#### The `<adjust-panner>` Element
Audio panning is controlled by the `<adjust-panner>` element, which is part of the intrinsic audio parameters (`%intrinsic-params-audio%`). It appears as a child of clip elements alongside `<adjust-volume>`.

```xml
<adjust-panner mode="stereo" amount="-50"/>
```

#### Attributes
*   **`mode`**: Panning mode (e.g., `"stereo"` for stereo left/right panning). Implied (optional).
*   **`amount`**: Pan position. Default: `"0"` (center). Range: `-100` (full left) to `100` (full right) for stereo.
*   **`original_decoded_mix`**, **`ambient_direct_mix`**: For surround sound decoding.
*   **`surround_width`**, **`left_right_mix`**, **`front_back_mix`**: Surround positioning controls.
*   **`LFE_balance`**: Low Frequency Effects channel balance.
*   **`rotation`**, **`stereo_spread`**: Additional surround spatialization.
*   **`attenuate_collapse_mix`**, **`center_balance`**: Surround downmix behavior.

#### Keyframed Panning
`<adjust-panner>` can contain `<param>` children for keyframing any of its values over time, just like other adjustment elements.

### 10.7 Retiming Edge Cases

#### Freeze Frames
A freeze frame holds a single source frame while timeline time advances. In `<timeMap>` terms, two `<timept>` entries with **the same `value`** but different `time` values create a hold:
```xml
<timeMap>
    <!-- Normal playback to 2s -->
    <timept time="0s" value="0s" interp="linear"/>
    <timept time="2s" value="2s" interp="linear"/>
    <!-- Freeze at source frame 2s for 3 seconds of timeline time -->
    <timept time="5s" value="2s" interp="linear"/>
    <!-- Resume normal playback -->
    <timept time="8s" value="5s" interp="linear"/>
</timeMap>
```
Between `time="2s"` and `time="5s"`, the source value stays at `2s` -- the same frame is displayed for 3 seconds.

#### Reverse Playback
Reverse playback occurs when the `value` (source time) **decreases** while `time` (timeline time) increases:
```xml
<timeMap>
    <timept time="0s" value="5s" interp="linear"/>
    <timept time="5s" value="0s" interp="linear"/>
</timeMap>
```
This plays 5 seconds of source media in reverse over 5 seconds of timeline time.

#### Variable Speed with Optical Flow
The `frameSampling` attribute on `<timeMap>` controls interpolation quality:
*   **`floor`** (default): Nearest frame, no interpolation. Fastest but choppy for slow motion.
*   **`nearest-neighbor`**: Similar to floor but with slightly different rounding.
*   **`frame-blending`**: Cross-dissolves between frames. Moderate quality.
*   **`optical-flow-classic`**: Motion-estimation based interpolation. High quality.
*   **`optical-flow`**: Improved optical flow algorithm. Highest quality.
*   **`optical-flow-frc`** (v1.11+): Frame Rate Conversion optimized optical flow.

#### Speed Ramp Easing
The `<timept>` element supports easing via `inTime` and `outTime` attributes, which control the smoothness of speed transitions at each keyframe. The `interp` attribute determines the curve type:
*   **`smooth2`** (default): Bezier-like smooth interpolation.
*   **`linear`**: Constant speed between points.
*   **`smooth`**: Legacy smooth interpolation.

**Gotcha**: Programmatically setting `inTime`/`outTime` values (e.g., `inTime="0.5" outTime="0.5"`) may not produce visible easing unless the values match what FCP's UI would generate. The exact easing behavior with these attributes is partially undocumented.

#### The timeMap Time Coordinate Space
The `time` attribute in `<timept>` is relative to the **clip's local timeline** (starting from 0 or the clip's `start` value). The `value` attribute maps to the **source media time**. The speed at any point is the slope of the time-to-value curve: `speed = delta(value) / delta(time)`.

### 10.8 Default Values Reference

A complete reference of important attribute defaults that parsers must assume when attributes are omitted.

| Attribute | Default | Notes |
|-----------|---------|-------|
| `offset` | `0s` | Position in parent timeline. Omitted = start of parent. |
| `start` | `0s` | DTD default is `0s`. For `asset-clip`, FCP may use the asset's `start` value from the resource. |
| `duration` | `#REQUIRED` on most clips | Must be specified on `<clip>`, `<gap>`, `<transition>`. Is `#IMPLIED` (optional) on `<asset-clip>` -- when omitted, uses the asset's full duration. |
| `lane` | `0` (implied) | `0` = contained in parent. Positive = above, negative = below. |
| `enabled` | `1` | Element is active by default. |
| `active` | `1` | Audio channel/role source is active by default. |
| `tcStart` | `#IMPLIED` | No default; when omitted, typically `0s` or inherited from parent sequence. |
| `tcFormat` | `#IMPLIED` | No default; when omitted, inherited from parent sequence or assumed NDF. |
| `srcEnable` | `"all"` | On `<ref-clip>`, `<asset-clip>`, `<mc-clip>`, `<mc-source>`: enables both audio and video. |
| `videoRole` | `"video"` | On `<asset-clip>`: default video role assignment. |
| `audioStart` | `#IMPLIED` | When omitted, audio uses the same `start` as video (no split edit). |
| `audioDuration` | `#IMPLIED` | When omitted, audio uses the same `duration` as video (no split edit). |
| `modDate` | `#IMPLIED` | Modification date. Optional on clips, projects, media, and auditions. |
| `interp` (timept) | `"smooth2"` | Default interpolation for speed mapping points. |
| `interp` (keyframe) | `"linear"` | Default interpolation for animation keyframes. |
| `frameSampling` | `"floor"` | Default frame sampling for retiming and conform-rate. |
| `scaleEnabled` | `1` | Rate conforming is active by default on `<conform-rate>`. |
| `preservesPitch` | `1` | Audio pitch is preserved during speed changes by default. |
| `amount` (adjust-blend) | `1.0` | Full opacity by default. |
| `amount` (adjust-panner) | `0` | Center pan by default. |
| `mode` (adjust-blend) | Omitted = Normal (`0`) | Normal compositing when not specified. |
| `type` (adjust-conform) | `"fit"` | Fit mode is assumed when `<adjust-conform>` is absent. |
| `inverted` (filter-video-mask) | `0` | Mask is not inverted by default. |
| `useAudioSubroles` (ref-clip) | `0` | Audio subroles are not used by default. |

### 10.9 Additional Gotchas

#### Frame Rate Conforming
When a clip's native frame rate differs from the sequence (e.g., 25fps clip in a 24fps timeline), a `<conform-rate>` element appears within the clip's timing parameters. The exact frame-by-frame mapping during conforming involves rounding that is not fully documented by Apple. Be cautious with frame-accurate timecode calculations across mixed frame rates.

#### Version Compatibility
*   FCPXML v1.3 renamed `<filter>` to `<filter-video>` and `<filter-audio>`.
*   FCPXML v1.3 replaced `<timeMap>` with `<conformRate>`, but later versions restored `<timeMap>`.
*   FCP 10.2 opens v1.5 but cannot open v1.6 (from FCP 10.3).
*   Always check the `version` attribute on `<fcpxml>` before parsing.

#### File Path Encoding
The `src` attribute on `<media-rep>` (and `url` on `<locator>`) uses standard percent-encoding. Spaces become `%20`. Always use proper URL encoding/decoding when reading or writing these attributes (e.g., `file:///Users/dev/My%20Project/clip.mov`).

#### Unicode in Markers
Marker `value` attributes can contain Unicode characters. Ensure your XML parser handles UTF-8 encoding correctly, and when writing FCPXML, use proper XML character escaping.

#### DTD Validation
FCPXML documents can be validated against the DTD bundled with Final Cut Pro:
```bash
xmllint --dtdvalid "/Applications/Final Cut Pro.app/Contents/Frameworks/Flexo.framework/Resources/FCPXMLv1_14.dtd" "/path/to/file.fcpxml"
```
This is the most reliable way to catch structural errors before importing into FCP.

#### The "Asset [nil]" Error
If FCP reports "Asset [nil] has no valid media" on import, the XML is malformed -- typically a missing or invalid `ref` attribute pointing to a nonexistent resource ID. Always validate that every `ref` in your timeline elements maps to a valid `id` in `<resources>`.

---

## 11. Titles, Text & Generators

### Titles
A `<title>` element is a text overlay that references an `<effect>` resource (such as "Basic Title" or "Custom Title"). Titles are effect-backed generators with styled text content.

#### **Title Structure**
```xml
<resources>
    <format id="r1" frameDuration="100/2400s" width="1920" height="1080"/>
    <effect id="r10" name="Basic Title" uid=".../Titles/Build In-Out/Basic Title"/>
</resources>

<title ref="r10" name="Welcome" offset="3600s" duration="5s" role="titles.titles-1">
    <!-- Generator parameters (position, alignment, etc.) -->
    <param name="Position" key="9999/999166631/999166633/1/100/101" value="0 -200"/>
    <param name="Alignment" key="9999/999166631/999166633/2/354/999169573/401" value="1 (Center)"/>

    <!-- The visible text content -->
    <text>
        <text-style ref="ts1">Hello World</text-style>
    </text>

    <!-- Style definition referenced by text-style -->
    <text-style-def id="ts1">
        <text-style font="Helvetica" fontSize="72" fontColor="1 1 1 1"
                    bold="1" alignment="center"/>
    </text-style-def>
</title>
```

#### **Text Elements**

*   **`<text>`**: Contains the visible text content as a mix of raw text and `<text-style>` elements.
    *   **`display-style`**: `pop-on` | `paint-on` | `roll-up` (for caption-style display).
    *   **`roll-up-height`**: Height for roll-up display.
    *   **`position`**: Text position override.
    *   **`placement`**: `left` | `right` | `top` | `bottom`.
    *   **`alignment`**: `left` | `center` | `right`.

*   **`<text-style-def>`**: Defines a reusable set of text style attributes, referenced by `<text-style>` elements via `ref`.
    *   **`id`**: Unique ID (required).
    *   **`name`**: Display name.
    *   Contains exactly one `<text-style>` child that defines the style properties.

*   **`<text-style>`**: Applies formatting to a run of text. Can appear inside `<text>` (wrapping text content) or inside `<text-style-def>` (defining the style).
    *   **`ref`**: IDREF pointing to a `<text-style-def>` — applies that style to the wrapped text.
    *   **`font`**: Font family name (e.g., `"Helvetica"`).
    *   **`fontSize`**: Size in points (e.g., `"72"`).
    *   **`fontFace`**: Font face variant (e.g., `"Bold"`).
    *   **`fontColor`**: RGBA color as `"R G B A"` (e.g., `"1 1 1 1"` for white).
    *   **`backgroundColor`**: Background color as `"R G B A"`.
    *   **`bold`** / **`italic`** / **`underline`**: `0` or `1`.
    *   **`strokeColor`**: Outline color as `"R G B A"`.
    *   **`strokeWidth`**: Outline width.
    *   **`baseline`** / **`baselineOffset`**: Vertical text offset.
    *   **`shadowColor`** / **`shadowOffset`** / **`shadowBlurRadius`**: Drop shadow properties.
    *   **`kerning`**: Character spacing.
    *   **`alignment`**: `left` | `center` | `right` | `justified`.
    *   **`lineSpacing`**: Line height adjustment.
    *   **`tabStops`**: Tab stop positions.

#### **Developer Example: Multi-Style Title**
*A title with two different text styles applied to different words.*

```xml
<title ref="r10" name="Styled Title" offset="3600s" duration="5s">
    <text>
        <text-style ref="ts1">Breaking </text-style>
        <text-style ref="ts2">News</text-style>
    </text>
    <text-style-def id="ts1">
        <text-style font="Helvetica" fontSize="48" fontColor="1 1 1 1"/>
    </text-style-def>
    <text-style-def id="ts2">
        <text-style font="Helvetica" fontSize="48" fontColor="1 0 0 1"
                    bold="1" italic="1"/>
    </text-style-def>
</title>
```

### Generators
Generators (solid colors, backgrounds, shapes, etc.) are **not a separate element type** in FCPXML. They use the `<video>` element with a `ref` pointing to an `<effect>` resource that has a generator `uid`.

#### **Developer Example: Solid Color Generator**
```xml
<resources>
    <effect id="r20" name="Custom" uid=".../Generators/Solid/Custom"/>
</resources>

<!-- In the timeline -->
<video ref="r20" name="Red Background" offset="3600s" duration="10s">
    <param name="Color" key="9999/1/100/101" value="1 0 0"/>
</video>
```

The `<video>` element's `ref` attribute accepts either an `<asset>` ID (for media clips) or an `<effect>` ID (for generators). The DTD uses `ref IDREF #REQUIRED`, which allows referencing either resource type.

#### **How to Distinguish Generators from Media Clips**
When parsing a `<video>` element, look up its `ref` in the resource dictionary:
*   If it points to an `<asset>` → it's a media clip.
*   If it points to an `<effect>` → it's a generator.
*   Similarly, `<title>` always references an `<effect>` — titles are specialized generators with text content.

---

## 12. The `param` Element & Effect Parameters

The `<param>` element is the universal mechanism for setting and animating values on effects, generators, titles, and adjustments. Understanding its attributes is essential for working with any FCPXML effect.

### **DTD Definition**
```
<!ELEMENT param (fadeIn?, fadeOut?, keyframeAnimation?, param*)>
<!ATTLIST param name CDATA #REQUIRED>
<!ATTLIST param key CDATA #IMPLIED>
<!ATTLIST param value CDATA #IMPLIED>
<!ATTLIST param auxValue CDATA #IMPLIED>
<!ATTLIST param enabled (0 | 1) "1">
```

### **Attributes**

*   **`name`** (required): Human-readable parameter name (e.g., `"Position"`, `"Opacity"`, `"Color"`). **Warning**: This value may be localized. A French-language FCP export might use `"Position"` → `"Position"` (same) but `"Amount"` → `"Quantité"`. Do not rely on `name` for programmatic identification — use `key` instead.

*   **`key`** (optional): Language-independent constant identifier. Uses a slash-delimited numeric path format (e.g., `"9999/999166631/999166633/1/100/101"`). If `key` is absent, `name` is used for identification. **Always prefer `key` over `name` when both are present.**

*   **`value`** (optional): The parameter value. The format depends on the parameter type:
    *   **Scalar**: A single number (e.g., `"100"`, `"0.75"`).
    *   **Point**: Space-separated coordinates (e.g., `"0 0"`, `"-50 25"`).
    *   **Color**: Space-separated RGBA (e.g., `"0.5 0.2 0.8 1.0"`).
    *   **Enumeration**: Integer with optional label (e.g., `"1 (Center)"`, `"2 (Right)"`). Parsers should extract only the integer.

*   **`auxValue`** (optional): Secondary value for two-value parameters. Rare — used by specific effects.

*   **`enabled`** (`0` | `1`, default: `1`): Whether the parameter is active.

### **Children**

*   **`<fadeIn>`**: Automatic fade-in applied to the parameter.
    *   **`type`**: `linear` | `easeIn` | `easeOut` | `easeInOut`.
    *   **`duration`**: Length of the fade.
*   **`<fadeOut>`**: Automatic fade-out (same attributes as `fadeIn`).
*   **`<keyframeAnimation>`**: Contains `<keyframe>` elements for animated values (see Section 5).
*   **`<param>*`**: Nested params for hierarchical parameter groups.

### **Nested Parameters**
Some effects expose compound parameters. The parent `<param>` acts as a group container, and nested `<param>` elements represent individual sub-controls:

```xml
<!-- Color Board effect with nested parameter groups -->
<filter-video ref="r8" name="Color Board">
    <param name="Color" key="colorBoard/color">
        <param name="Global Amount" key="colorBoard/color/global/amount" value="0"/>
        <param name="Highlights Color" key="colorBoard/color/highlights/color" value="0.1 -0.05"/>
        <param name="Midtones Color" key="colorBoard/color/midtones/color" value="0 0"/>
        <param name="Shadows Color" key="colorBoard/color/shadows/color" value="-0.1 0.05"/>
    </param>
</filter-video>
```

### **Value Priority**
When a parameter has multiple sources, priority is (highest to lowest):
1.  **Keyframe value** at the current time (from `<keyframeAnimation>`).
2.  **`<param>` element's `value` attribute** (static value).
3.  **Adjustment element's attribute** (e.g., `position` on `<adjust-transform>`).

### **The `<data>` Element**

The `<data>` element stores arbitrary data chunks, typically used by effects and match EQ profiles.

*   **`key`**: Identifier for the data (e.g., `effectData`, `effectConfig`).
*   Content: `#PCDATA` (raw data).

### **Supporting Elements**

*   **`<bookmark>`**: A `#PCDATA` element that stores file bookmark data. Appears as a child of `<media-rep>` or `<locator>`.
*   **`<reserved>`**: Internal use element containing `#PCDATA`. May appear in `<adjust-blend>` or `<transition>`.
*   **`<array>`**: Container for array values in `<md>` metadata. Contains `<string>` children.
*   **`<string>`**: Text content (`#PCDATA`) within an `<array>`.

### **Developer Example: Finding the Right `key`**
The easiest way to discover the `key` for a specific parameter is:
1.  Create a timeline in FCP with the desired effect.
2.  Set the parameter to a recognizable value.
3.  Export as FCPXML (`File > Export XML`).
4.  Search the exported XML for your value — the surrounding `<param>` will show the `key`.

---

## 13. Color Processing & HDR

### Library-Level Color Processing
The `colorProcessing` attribute on `<library>` controls how the entire library handles color:

*   **`standard`**: Standard Dynamic Range (SDR), Rec. 709 color space.
*   **`wide`**: Wide Color Gamut, Rec. 2020 color space (no HDR).
*   **`wide-hdr`**: Wide Color Gamut with High Dynamic Range (Rec. 2020 PQ or HLG).

```xml
<library location="file:///path/to/Library.fcpbundle/" colorProcessing="wide-hdr">
    ...
</library>
```

### Format Color Space
The `colorSpace` attribute on `<format>` defines the render color space for that format:

*   `"1-1-1 (Rec. 709)"` — Standard HD color space.
*   `"Rec. 2020"` — Wide gamut SDR.
*   `"Rec. 2020 PQ"` — HDR with Perceptual Quantizer (HDR10/Dolby Vision).
*   `"Rec. 2020 HLG"` — HDR with Hybrid Log-Gamma (broadcast HDR).

### Asset Color Space Override
The `colorSpaceOverride` attribute on `<asset>` allows overriding the auto-detected color space of source media. This is useful when FCP misidentifies a clip's color space:

```xml
<asset id="r2" name="LOG Footage"
       start="0s" duration="100s" hasVideo="1" hasAudio="1"
       colorSpaceOverride="Rec. 2020 HLG">
    <media-rep kind="original-media" src="file:///media/clip.mov"/>
</asset>
```

### Color Conforming (`<adjust-colorConform>`)
Introduced in FCPXML v1.11, this element handles automatic tone mapping when mixing HDR and SDR content in the same timeline.

*   **`enabled`**: `0` or `1` (default: `1`).
*   **`autoOrManual`**: `automatic` | `manual` (default: `automatic`).
*   **`conformType`**: The conversion applied:
    *   `conformNone` (default) — No conversion.
    *   `conformAuto` — FCP chooses automatically.
    *   `conformHLGtoSDR` / `conformPQtoSDR` — HDR to SDR tone mapping.
    *   `conformHLGtoPQ` / `conformPQtoHLG` — Cross-HDR format conversion.
    *   `conformSDRtoHLG75` / `conformSDRtoHLG100` / `conformSDRtoPQ` — SDR to HDR uplift.
*   **`peakNitsOfPQSource`**: Peak brightness of PQ source in nits (required).
*   **`peakNitsOfSDRToPQSource`**: Peak brightness for SDR-to-PQ mapping (required).

#### **Developer Example: HDR Timeline with SDR Clip**
```xml
<resources>
    <format id="r1" frameDuration="100/2400s" width="3840" height="2160"
            colorSpace="Rec. 2020 HLG"/>
    <asset id="r2" name="SDR Interview"
           start="0s" duration="60s" hasVideo="1" hasAudio="1">
        <media-rep kind="original-media" src="file:///media/interview.mov"/>
    </asset>
</resources>

<sequence format="r1" duration="60s">
    <spine>
        <asset-clip ref="r2" offset="0s" duration="60s">
            <!-- Uplift SDR content to HLG for the HDR timeline -->
            <adjust-colorConform conformType="conformSDRtoHLG100"
                                 peakNitsOfPQSource="1000"
                                 peakNitsOfSDRToPQSource="203"/>
        </asset-clip>
    </spine>
</sequence>
```

---

## 14. Crop Modes & Ken Burns Effect

The `<adjust-crop>` element supports three distinct modes, each with its own child elements.

### **Mode: `trim`**
Simple edge trimming. Uses a `<trim-rect>` child with inset values.

```xml
<adjust-crop mode="trim" enabled="1">
    <trim-rect left="0.1" top="0.05" right="0.1" bottom="0.05"/>
</adjust-crop>
```

*   **`<trim-rect>`**: Defines inset amounts from each edge. Values are expressed as a **percentage of original frame height**.
    *   **`left`** / **`top`** / **`right`** / **`bottom`**: Inset values as percentage of frame height. Default: `0` for all.
    *   Can contain `<param>` children for keyframed animation (same as `<crop-rect>`).

### **Mode: `crop`**
Allows keyframed cropping via `<crop-rect>` with nested `<param>` elements.

```xml
<adjust-crop mode="crop" enabled="1">
    <crop-rect left="0.1" top="0.1" right="0.1" bottom="0.1">
        <param name="left">
            <keyframeAnimation>
                <keyframe time="0s" value="0.1"/>
                <keyframe time="5s" value="0.3"/>
            </keyframeAnimation>
        </param>
    </crop-rect>
</adjust-crop>
```

*   **`<crop-rect>`**: Same attributes as `<trim-rect>` (values as percentage of original frame height), but supports `<param>` children for animation.

### **Mode: `pan` (Ken Burns Effect)**
Animates between a start and end crop rectangle, creating a pan-and-zoom effect commonly used for still images.

```xml
<adjust-crop mode="pan" enabled="1">
    <!-- Start rectangle (what you see at the beginning) -->
    <pan-rect left="0" top="0" right="0.5" bottom="0.5"/>
    <!-- End rectangle (what you see at the end) -->
    <pan-rect left="0.3" top="0.2" right="0.2" bottom="0.3"/>
</adjust-crop>
```

*   **`<pan-rect>`**: Defines a crop rectangle for one end of the animation.
    *   **`left`** / **`top`** / **`right`** / **`bottom`**: Fractional inset values (default: `0`).
*   The **first** `<pan-rect>` child is the start state.
*   The **second** `<pan-rect>` child is the end state.
*   FCP interpolates between them over the clip's duration.
*   `<pan-rect>` does **not** support `<param>` children — the animation is implicit between the two rectangles.

### **Still Images & Photos**
Still images are handled as regular `<asset>` / `<asset-clip>` elements with `hasVideo="1" hasAudio="0"`. Key differences:
*   The `duration` on the timeline `<asset-clip>` controls how long the photo is displayed (any duration is valid since there's no intrinsic playback length).
*   Ken Burns (`mode="pan"`) is the most common adjustment for photos.
*   `<adjust-conform>` with `type="fit"` or `type="fill"` controls how the image fits the frame when aspect ratios differ.

---

## 15. Object Tracking, Stabilization & Additional Adjustments

### Object Tracking

Object tracking (introduced in FCPXML v1.10 / FCP 10.6) allows effects and transforms to follow a tracked object's motion automatically.

#### **Elements**

*   **`<object-tracker>`**: Container for tracking shapes. Appears as a child of clip elements alongside other adjustments.
    *   Children: one or more `<tracking-shape>` elements.

*   **`<tracking-shape>`**: Defines a single tracked region.
    *   **`id`** (required): Unique ID, referenced by `tracking` attributes on other elements.
    *   **`name`**: Display name (e.g., `"Face"`, `"Car"`).
    *   **`offsetEnabled`**: `0` | `1` (default: `0`). Whether offset tracking is enabled.
    *   **`analysisMethod`**: `automatic` (default) | `combined` | `machineLearning` | `pointCloud`.
    *   **`dataLocator`**: IDREF referencing tracking data stored externally (used in `.fcpxmld` bundles).
    *   This is an **empty element** (no children).

#### **Linking Tracking to Transforms**
The `tracking` attribute on `<adjust-transform>` (and `<mask-shape>`) accepts an IDREF pointing to a `<tracking-shape>`:

```xml
<asset-clip ref="r2" offset="3600s" duration="10s">
    <!-- Define the tracker -->
    <object-tracker>
        <tracking-shape id="track1" name="Face" analysisMethod="machineLearning"/>
    </object-tracker>

    <!-- Link transform to the tracked object -->
    <adjust-transform position="0 10" tracking="track1"/>
</asset-clip>
```

When `tracking` is set, the transform follows the tracked object's motion, and the `position` value acts as an offset from the tracked position.

### Stabilization

#### **`<adjust-stabilization>`**
Controls video stabilization. Part of the intrinsic video parameters.

*   **`enabled`**: `0` | `1` (default: `1`).
*   **`type`**: The stabilization algorithm:
    *   `automatic` (default) — FCP chooses the best method.
    *   `inertiaCam` — Smooths camera movement while preserving general direction (handheld shots with intentional movement).
    *   `smoothCam` — Aggressive stabilization for tripod-like stillness.

```xml
<asset-clip ref="r2" offset="3600s" duration="10s">
    <adjust-stabilization type="inertiaCam" enabled="1"/>
</asset-clip>
```

#### **`<adjust-rollingShutter>`**
Corrects rolling shutter distortion (jello effect) from CMOS sensors.

*   **`enabled`**: `0` | `1` (default: `1`).
*   **`amount`**: Correction strength: `none` (default) | `low` | `medium` | `high` | `extraHigh`.

```xml
<asset-clip ref="r2" offset="3600s" duration="10s">
    <adjust-rollingShutter amount="medium" enabled="1"/>
</asset-clip>
```

### Additional Adjustment Elements

These elements exist in the DTD but are less commonly encountered. Included here for completeness.

#### **Video Adjustments**

*   **`<adjust-360-transform>`**: Spatial transformations for 360° video.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`coordinates`** (required): `spherical` | `cartesian`.
    *   **Spherical coordinates**: `latitude` (default `0`), `longitude` (default `0`), `distance`.
    *   **Cartesian coordinates**: `xPosition` (default `0`), `yPosition` (default `0`), `zPosition`.
    *   **`xOrientation`** / **`yOrientation`** / **`zOrientation`**: Orientation angles (all default `0`).
    *   **`autoOrient`**: `0` | `1` (default: `1`).
    *   **`convergence`**: Stereo convergence (default `0`).
    *   **`interaxial`**: Stereo interaxial distance.
    *   **`scale`**: Scale factor (default `1 1`).
    *   Contains `<param>` children for keyframing.

*   **`<adjust-reorient>`**: Reorients the horizon in 360° video.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`tilt`** / **`pan`** / **`roll`**: Rotation angles (all default `0`).
    *   **`convergence`**: Stereo convergence (default `0`).
    *   Contains `<param>` children.

*   **`<adjust-orientation>`**: Adjusts the viewing orientation for 360° video.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`tilt`** / **`pan`** / **`roll`**: Rotation angles (all default `0`).
    *   **`fieldOfView`**: Field of view angle.
    *   **`mapping`**: `normal` (default) | `tinyPlanet`.
    *   Contains `<param>` children.

*   **`<adjust-cinematic>`** (v1.11+): Controls Cinematic Mode focus editing for iPhone footage.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`dataLocator`**: IDREF referencing external focus data via a `<locator>` resource.
    *   **`aperture`**: Virtual aperture value.
    *   Contains `<param>` children for focus adjustments.

*   **`<adjust-stereo-3D>`** (v1.13+): Controls spatial video (stereoscopic 3D) properties.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`convergence`**: Stereo convergence (default `0`).
    *   **`autoScale`**: `0` | `1` (default: `1`).
    *   **`swapEyes`**: `0` (default) | `1`.
    *   **`depth`**: Depth adjustment (default `0`).
    *   Contains `<param>` children.

*   **`<adjust-corners>`**: Four-corner pin distortion (perspective transforms).
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`botLeft`** / **`topLeft`** / **`topRight`** / **`botRight`**: Corner positions as `"x y"` (all default `"0 0"`).
    *   Contains `<param>` children for each corner position.

#### **Audio Adjustments**

*   **`<adjust-loudness>`**: Automatic loudness correction.
    *   **`amount`** (required): Target loudness level.
    *   **`uniformity`** (required): Loudness uniformity setting.

*   **`<adjust-noiseReduction>`**: Background noise removal.
    *   **`amount`** (required): Reduction strength.

*   **`<adjust-humReduction>`**: Removes electrical hum (50/60 Hz).
    *   **`frequency`** (required): `50` | `60`. The hum frequency to target.

*   **`<adjust-EQ>`**: Parametric equalizer.
    *   **`mode`** (required): `flat` | `voice_enhance` | `music_enhance` | `loudness` | `hum_reduction` | `bass_boost` | `bass_reduce` | `treble_boost` | `treble_reduce`.
    *   Contains `<param>` children for band-specific adjustments.

*   **`<adjust-matchEQ>`**: Matches the EQ profile of one clip to another. Contains a `<data>` child.

*   **`<adjust-voiceIsolation>`** (v1.11+): AI-powered voice isolation that separates speech from background noise.
    *   **`amount`** (required): Isolation strength.

*   **`<mute>`**: Suppresses audio output for a range of source media time.
    *   **`start` / `duration`**: The muted time range.
    *   Can contain optional `<fadeIn>` and `<fadeOut>` children for smooth transitions.

#### **Masking**

*   **`<filter-video-mask>`**: Applies a video filter through a mask shape.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`inverted`**: `0` (default) | `1`. Whether the mask is inverted.
    *   Contains one or more `<mask-shape>` or `<mask-isolation>` elements, followed by one or two `<filter-video>` elements (second is for outer color correction only).

*   **`<mask-shape>`**: Defines a shape mask.
    *   **`name`**: Display name.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`blendMode`**: `add` (default) | `subtract` | `multiply`.
    *   **`tracking`**: IDREF linking to an `<object-tracker>` for tracked masks.
    *   Contains `<param>` children.

*   **`<mask-isolation>`** (v1.8+): Isolation mask for color-based selections.
    *   **`name`**: Display name.
    *   **`enabled`**: `0` | `1` (default: `1`).
    *   **`blendMode`**: `add` | `subtract` | `multiply` (default: `multiply`).
    *   **`type`**: `3D` (default) | `HSL`.
    *   Contains a `<data>` child and optional `<param>` children.

---

## 16. Additional Developer Examples

### 16.1 Live Drawing
Live drawings (`<live-drawing>`) are vector graphics animations introduced in v1.11. They reference external drawing data via a `<locator>` resource.

```xml
<resources>
    <locator id="r15" url="file:///path/to/drawing-data.pkdrawing"/>
</resources>

<!-- In a spine or as a connected clip -->
<live-drawing lane="1" offset="3600s" duration="5s" role="video"
             dataLocator="r15" animationType="replay"/>
```

**Attributes:**
*   **`role`**: Role assignment (default: `video`).
*   **`dataLocator`**: IDREF referencing a `<locator>` resource for the serialized PKDrawing data file.
*   **`animationType`**: Animation playback mode.
*   Standard clip attributes (`offset`, `duration`, `lane`, `enabled`, etc.).
*   Supports a subset of intrinsic video params: `adjust-crop`, `adjust-corners`, `adjust-conform`, `adjust-transform`, `adjust-blend`, `adjust-360-transform`, `adjust-colorConform`, `adjust-stereo-3D`.

### 16.2 Ratings and Keywords with Notes

```xml
<asset-clip ref="r2" offset="3600s" duration="30s">
    <!-- Mark a range as a favorite with a note -->
    <rating start="5s" duration="10s" value="favorite" note="Great expression"/>

    <!-- Reject a bad section -->
    <rating start="25s" duration="5s" value="reject"/>

    <!-- Tag a range with keywords -->
    <keyword start="0s" duration="30s" value="interview, wide shot"
             note="Main angle, good lighting"/>
</asset-clip>
```

### 16.3 Analysis Markers with Shot Types

```xml
<asset-clip ref="r2" offset="3600s" duration="60s">
    <!-- FCP's automatic analysis results -->
    <analysis-marker start="0s" duration="10s">
        <shot-type value="wideShot"/>
    </analysis-marker>
    <analysis-marker start="10s" duration="15s">
        <shot-type value="onePerson"/>
        <shot-type value="closeUp"/>
    </analysis-marker>
    <analysis-marker start="40s" duration="5s">
        <stabilization-type value="excessiveShake"/>
    </analysis-marker>
</asset-clip>
```

### 16.4 Mute with Fades

```xml
<audio-channel-source srcCh="1, 2" outCh="L, R" role="dialogue">
    <!-- Suppress audio from 5s to 7s with smooth fades -->
    <mute start="5s" duration="2s">
        <fadeIn type="easeIn" duration="100/2400s"/>
        <fadeOut type="easeOut" duration="100/2400s"/>
    </mute>
</audio-channel-source>
```

### 16.5 Adjust-Corners (Perspective Transform)

```xml
<!-- Pin the four corners to create a perspective effect -->
<adjust-corners enabled="1"
    botLeft="-480 -270" topLeft="-480 270"
    topRight="480 270" botRight="480 -270">
    <!-- Animate bottom-left corner -->
    <param name="botLeft">
        <keyframeAnimation>
            <keyframe time="0s" value="-480 -270" interp="ease"/>
            <keyframe time="5s" value="-400 -200"/>
        </keyframeAnimation>
    </param>
</adjust-corners>
```

### 16.6 Audio Enhancements

```xml
<asset-clip ref="r2" offset="3600s" duration="30s">
    <audio-channel-source srcCh="1, 2" outCh="L, R" role="dialogue">
        <!-- Normalize loudness -->
        <adjust-loudness amount="-14" uniformity="0.5"/>

        <!-- Remove background noise -->
        <adjust-noiseReduction amount="50"/>

        <!-- Remove 60Hz electrical hum -->
        <adjust-humReduction frequency="60"/>

        <!-- Apply voice enhancement EQ -->
        <adjust-EQ mode="voice_enhance"/>

        <!-- Isolate voice from background -->
        <adjust-voiceIsolation amount="50"/>
    </audio-channel-source>
</asset-clip>
```

### 16.7 Marker as To-Do Item

```xml
<asset-clip ref="r2" offset="3600s" duration="10s">
    <!-- Standard marker -->
    <marker start="2s" value="Great expression here"/>

    <!-- To-do marker (not yet completed) -->
    <marker start="5s" value="Fix color here" completed="0"
            note="Skin tones look too warm"/>

    <!-- Completed to-do marker -->
    <marker start="8s" value="Audio level fixed" completed="1"/>
</asset-clip>
```

### 16.8 Metadata with Types and Arrays

```xml
<metadata>
    <md key="com.apple.proapps.studio.reel" value="A001"
        type="string" editable="1" displayName="Reel"/>
    <md key="com.apple.proapps.studio.scene" value="Scene 5"
        type="string" editable="1"/>
    <md key="com.example.custom.tags" type="string" displayName="Tags">
        <array>
            <string>outdoor</string>
            <string>sunset</string>
            <string>drone</string>
        </array>
    </md>
</metadata>
```

---

## 17. Smart Collections & Organization

FCPXML supports a rich system for organizing clips through keyword collections, smart collections with rule-based matching, and collection folders.

### **Collection Elements**

*   **`<keyword-collection>`**: A simple collection based on a keyword.
    *   **`name`** (required): The keyword name.
*   **`<collection-folder>`**: A folder for organizing collections.
    *   **`name`** (required): The folder name.
    *   Contains: `<collection-folder>`, `<keyword-collection>`, and/or `<smart-collection>` children.
*   **`<smart-collection>`**: A rule-based collection that automatically matches clips.
    *   **`name`** (required): Display name.
    *   **`match`** (required): `any` | `all`. Whether clips must match any or all rules.

### **Match Rules**

Smart collections contain one or more match rule elements:

| Element | Key Attributes | Description |
|---|---|---|
| `<match-text>` | `rule` (includes\|doesNotInclude\|is\|isNot\|startsWith\|endsWith\|isRelatedTo), `value`, `scope` (all\|notes\|names\|markers\|transcript\|visual\|all-text) | Text search |
| `<match-ratings>` | `value` (favorites\|rejected) | Rating filter |
| `<match-media>` | `rule` (is\|isNot), `type` (videoWithAudio\|videoOnly\|audioOnly\|stills) | Media type filter |
| `<match-clip>` | `rule` (is\|isNot), `type` (audition\|synchronized\|compound\|multicam\|layeredGraphic\|project) | Clip type filter |
| `<match-stabilization>` | `rule` (includesAny\|includesAll\|doesNotIncludeAny\|doesNotIncludeAll) | Camera shake filter. Contains `<stabilization-type>` children. |
| `<match-keywords>` | `rule` (includesAny\|includesAll\|doesNotIncludeAny\|doesNotIncludeAll) | Keyword filter. Contains `<keyword-name>` children. |
| `<match-shot>` | `rule` (includesAny\|includesAll\|doesNotIncludeAny\|doesNotIncludeAll) | Shot type filter. Contains `<shot-type>` children. |
| `<match-property>` | `key` (reel\|scene\|take\|audioOutputChannels\|frameSize\|videoFrameRate\|audioSampleRate\|cameraName\|cameraAngle\|projection\|stereoscopic\|cinematic), `rule`, `value` | Property metadata filter |
| `<match-time>` | `type` (contentCreated\|dateImported), `rule` (is\|isBefore\|isAfter), `value` | Exact time filter |
| `<match-timeRange>` | `type` (contentCreated\|dateImported), `rule` (isInLast\|isNotInLast), `value`, `units` (hour\|day\|week\|month\|year) | Relative time range filter |
| `<match-roles>` | `rule` (includesAny\|includesAll\|doesNotIncludeAny\|doesNotIncludeAll) | Role filter. Contains `<role>` children (`name` attr). |
| `<match-usage>` | `rule` (used\|unused) | Usage status filter |
| `<match-representation>` | `type` (original\|optimized\|proxy), `rule` (isAvailable\|isMissing) | Media representation filter |
| `<match-markers>` | `type` (all\|standard\|allTodo\|complete\|incomplete) | Marker presence filter |
| `<match-analysis-type>` | `rule` (isAvailable\|isMissing), `value` (any\|transcript\|visual) | Analysis data filter (v1.14: `transcript`/`visual` values) |

All match rules have an `enabled` attribute (`0` | `1`, default: `1`).

### **Developer Example: Smart Collections**

```xml
<library location="file:///Users/dev/Movies/MyProject.fcpbundle/">
    <!-- A smart collection that finds all favorited wide shots -->
    <smart-collection name="Favorite Wide Shots" match="all">
        <match-ratings enabled="1" value="favorites"/>
        <match-shot enabled="1" rule="includesAny">
            <shot-type value="wideShot"/>
        </match-shot>
    </smart-collection>

    <!-- A smart collection for unused interview clips -->
    <smart-collection name="Unused Interviews" match="all">
        <match-usage enabled="1" rule="unused"/>
        <match-keywords enabled="1" rule="includesAny">
            <keyword-name value="interview"/>
        </match-keywords>
    </smart-collection>

    <!-- Collection folder for organization -->
    <collection-folder name="Selects">
        <keyword-collection name="B-Roll"/>
        <smart-collection name="Recent Imports" match="all">
            <match-timeRange enabled="1" type="dateImported"
                             rule="isInLast" value="7" units="day"/>
        </smart-collection>
    </collection-folder>

    <event name="Day 1">
        <!-- ... clips ... -->
    </event>
</library>
```

---

## 18. Version History & Compatibility

### FCPXML Version to Final Cut Pro Mapping

| FCPXML Version | FCP Version | Key Changes |
|---|---|---|
| 1.7 | FCP 10.4 | Closed captions (`<caption>`, CEA-608, iTT) |
| 1.8 | FCP 10.4.4 | `<mask-isolation>`, shape/effect mask values via XML |
| 1.9 | FCP 10.5.x | Updates to existing elements |
| 1.10 | FCP 10.6 | `.fcpxmld` bundle format, `<object-tracker>`, `<tracking-shape>`, `tracking` attribute |
| 1.11 | FCP 10.6.6 | `<adjust-colorConform>`, `<adjust-cinematic>`, `<live-drawing>`, `<adjust-voiceIsolation>`, `optical-flow-frc` frame sampling |
| 1.12 | FCP 10.8 | `nameOverride` attribute on filters, additional metadata features |
| 1.13 | FCP 11.0 | `<adjust-stereo-3D>`, spatial video attributes (`stereoscopic`, `heroEye`), `<hidden-clip-marker>`, 90/100/120fps conform rates |
| 1.14 | FCP 12.0 | `<match-analysis-type>` gains `transcript`/`visual` values, `<match-text>` scope gains `transcript`/`visual`/`all-text`, beat detection support |

### Breaking Changes & Migration Notes

*   **v1.3**: Renamed `<filter>` to `<filter-video>` and `<filter-audio>`. Replaced `<timeMap>` with `<conformRate>` (later versions restored `<timeMap>`).
*   **v1.5 → v1.6**: Not backward-compatible. FCP 10.2 opens v1.5 but cannot open v1.6 (from FCP 10.3).
*   **v1.10**: Introduced the `.fcpxmld` bundle format — a directory containing the XML file plus external tracking data files. Single-file `.fcpxml` is still supported.

### Parser Recommendations
1.  Always check the `version` attribute on the `<fcpxml>` root element before parsing.
2.  Gracefully handle unknown elements and attributes from newer versions — skip them rather than failing.
3.  FCP can export older versions (e.g., FCP 12 can export as v1.14, v1.13, v1.12, or v1.11). If you only support a specific version, instruct users to export in that version.
4.  When generating FCPXML, target the **oldest version** that supports the features you need, for maximum compatibility.

### DTD Validation
The DTD files are bundled with Final Cut Pro. Validate before import:
```bash
xmllint --dtdvalid "/Applications/Final Cut Pro.app/Contents/Frameworks/Interchange.framework/Versions/A/Resources/FCPXMLv1_14.dtd" "/path/to/file.fcpxml"
```

DTD files are also available on GitHub for reference (e.g., in the CommandPost and cutlass repositories).

---

## 19. Import Options

The `<import-options>` element appears as an optional first child of `<fcpxml>`, before `<resources>`. It controls how FCP processes the file during import.

### **Structure**
```xml
<fcpxml version="1.14">
    <import-options>
        <option key="copy assets" value="1"/>
        <option key="assign audio role" value="dialogue"/>
    </import-options>
    <resources>
        ...
    </resources>
    ...
</fcpxml>
```

### **Elements**

*   **`<import-options>`**: Container for import option key-value pairs.
    *   Children: zero or more `<option>` elements.

*   **`<option>`**: A single import option.
    *   **`key`** (required): The option identifier.
    *   **`value`** (required): The option value.

### **Important Note**
FCPXML is an **interchange/import** format, not an export-configuration format. There are no elements for controlling export settings, share destinations, or render settings. Export configuration is handled by FCP's share destinations system or the Compressor application. The `renderFormat` attribute on `<sequence>` specifies the codec used for preview render files within FCP's library, but does **not** control export format.

---

## 20. UltimateSlice ↔ Final Cut Pro Compatibility

This section documents the specific requirements and implementation details for producing `.fcpxml` files that Final Cut Pro can import without errors. These rules were discovered through iterative testing against FCP 12 and are implemented in the strict writer (`write_fcpxml_strict`).

### 20.1 Save Routing

| Extension | Writer | Description |
|---|---|---|
| `.fcpxml` | `write_fcpxml_strict` | Strict DTD-compliant output for FCP import. Omits `us:*` vendor attributes, unknown passthrough fields, and sequence markers. |
| `.uspxml` | `write_fcpxml` | Feature-rich output with vendor extensions for lossless UltimateSlice round-trip. |

The packaged export (`export_project_with_media`) always uses the strict writer.

### 20.2 Rational Time Format

FCPXML uses rational numbers for time, expressed as `numerator/denominator s` (result in seconds).

**Integer frame rates** (24, 25, 30, 50, 60 fps): `frames / fps s`
```
Frame 48 at 24fps → 48/24s
```

**NTSC frame rates** (23.976, 29.97, 59.94 fps): `frames × denom / (fps_num) s`, where the rate is `fps_num / denom` (e.g., 24000/1001). The frame count must be multiplied by `denom`:
```
Frame 119 at 23.976fps → 119 × 1001 / 24000 = 119119/24000s
```

**Rounding**: When converting nanoseconds to FCPXML time at NTSC rates, round to the nearest frame (not truncate):
```
frames = (ns × timebase + denom × 500_000_000) / (denom × 1_000_000_000)
```

### 20.3 Asset `start` Must Match Embedded Timecode

FCP validates that each asset's `start` attribute matches the media file's embedded timecode. A mismatch produces: *"Invalid edit with no respective media."*

**Video files**: Extract timecode via ffprobe:
```bash
ffprobe -v quiet -select_streams v:0 -show_entries stream_tags=timecode -of csv=p=0 file.mp4
# Output: 20:13:33:07
```
Convert `HH:MM:SS:FF` to nanoseconds using the media's frame rate, then to FCPXML rational time.

**Audio files (WAV/BWF)**: Extract the BWF `time_reference` tag (sample offset):
```bash
ffprobe -v quiet -show_entries format_tags=time_reference -of csv=p=0 file.wav
# Output: 172910769
```
Convert samples to nanoseconds: `time_reference × 1_000_000_000 / sample_rate`. Use a 48 kHz time base (e.g., `samples/48000s`), not the video frame rate.

**Fallback**: When no embedded timecode is found, use `start="0s"`.

### 20.4 Per-Asset Format Generation

FCP requires each asset's `format` to match the media's actual frame rate and resolution. A 24 fps project containing 23.976 fps GoPro media must emit separate `<format>` elements:

```xml
<format id="r1" frameDuration="100/2400s" width="1920" height="1080"/>   <!-- project: 24fps -->
<format id="r2" frameDuration="1001/24000s" width="1920" height="1080"/> <!-- GoPro: 23.976fps -->
```

The strict writer probes each media file at export time (`build_export_context`) and assigns per-asset format IDs. Known FCP format names (e.g., `FFVideoFormat1080p2398`) are used when the resolution and frame rate match a standard.

> **Note**: GoPro cameras labelled "24 fps" actually record at 24000/1001 (23.976 fps).

### 20.5 Audio-Only Assets

Audio-only files (WAV, MP3, etc.) require special handling:

| Attribute | Value |
|---|---|
| Format | `FFVideoFormatRateUndefined` — a special `<format>` with no `frameDuration`, `width`, or `height` |
| `hasAudio` | `"1"` |
| `hasVideo` | Omitted entirely (not `"0"`) |
| `audioSources` | `"1"` |
| `audioChannels` | `"2"` (stereo) |
| `audioRate` | `"48000"` or actual sample rate |
| `start` | Derived from BWF `time_reference` (Section 20.3) in 48 kHz time base |
| `duration` | Probed via ffprobe, expressed in 48 kHz time base |
| Asset-clip `format` | Must reference the `FFVideoFormatRateUndefined` format ID |
| Asset-clip `tcFormat` | Omitted |
| Asset-clip `start`/`duration` | Expressed in 48 kHz time base (e.g., `0/48000s`), NOT video frame rate |

**Critical**: Audio time values must NOT use video frame denominators. For example, `0/24s` will fail for a `FFVideoFormatRateUndefined` format.

### 20.6 Connected Clips

FCP requires connected clips (lane ≠ 0) to be **nested inside** the primary storyline clip they connect to, not as flat siblings in the spine:

```xml
<spine>
  <asset-clip ref="a1" offset="0s" duration="10s">       <!-- primary -->
    <asset-clip ref="a2" lane="-1" offset="3s" .../>      <!-- connected (audio below) -->
    <asset-clip ref="a3" lane="1" offset="5s" .../>       <!-- connected (video above) -->
  </asset-clip>
</spine>
```

The connected clip's `offset` is in the **parent clip's source time space**, not the timeline. Given a connected clip at timeline position `T`, nested inside a primary clip with timeline start `P_timeline` and source start `P_source`:
```
offset = P_source + (T - P_timeline)
```

### 20.7 Volume & Pan Keyframes in `<audio-channel-source>`

FCP wraps keyframed volume and pan adjustments inside `<audio-channel-source>` elements. This structure is required for FCP to recognize the keyframes on import.

**FCP's format**:
```xml
<asset-clip ref="a1" offset="0s" start="1734122390/24000s" duration="10s">
  <audio-channel-source srcCh="1, 2" role="dialogue">
    <adjust-volume>
      <param name="amount">
        <keyframeAnimation>
          <keyframe time="52023671700/720000s" value="0dB"/>
          <keyframe time="52024956360/720000s" value="-14.5dB"/>
        </keyframeAnimation>
      </param>
    </adjust-volume>
  </audio-channel-source>
</asset-clip>
```

**Import**: The parser extracts `<adjust-volume>` and `<adjust-panner>` keyframes from inside `<audio-channel-source>` and applies them to the clip model.

**Export (strict)**: When a clip has volume or pan keyframes, the strict writer wraps them in `<audio-channel-source srcCh="1, 2" role="dialogue">`. Flat (non-keyframed) volume and pan are emitted as direct `<adjust-volume amount="..."/>` and `<adjust-panner amount="..."/>` children of the asset-clip.

### 20.8 Keyframe Time Coordinate Space

FCP emits keyframe `time` values in **absolute source media time** — the same coordinate space as the asset-clip's `start` attribute. This means keyframes for a clip whose media starts at timecode `20:13:33:07` will have times around 72813 seconds, not 0.

UltimateSlice stores keyframes in **clip-local time** (0 = clip start). The conversion is:

**Import (FCP → UltimateSlice)**:
```
clip_local_time = keyframe_time - source_in
```
Where `source_in` is the clip's source in-point parsed from the `start` attribute.

**Export (UltimateSlice → FCP)**:
```
fcpxml_time = clip_local_time + source_start_ns
```
Where `source_start_ns` is the same value written to the asset-clip's `start` attribute (timecode + source_in, frame-aligned).

**Example**: GoPro clip with embedded timecode `20:13:33:07` at 23.976 fps:
- Asset-clip `start="1734122390/24000s"` ≈ 72255.1s
- FCP keyframes at `52023671700/720000s` (≈72255.1s), `52024956360/720000s` (≈72256.9s)
- After subtracting `source_in`: clip-local keyframes at 0s, 1.784s — within the clip's duration

> **Overflow note**: FCP uses very large numerators (e.g., `52335350310/720000s`). The parser must use `u128` intermediate math when multiplying by 10⁹ to convert to nanoseconds, as `u64` overflows.

### 20.9 Strict Writer Element Order

The strict writer emits intrinsic parameters in DTD order within each `<asset-clip>`:

1. `<timeMap>` (if present)
2. `<adjust-crop>` / `<crop-rect>` (video only)
3. `<adjust-transform>` (video only)
4. `<adjust-blend>` (video only, with opacity keyframes if present)
5. `<audio-channel-source>` (only when volume/pan keyframes exist)
6. `<adjust-volume>` (flat, only when no volume keyframes)
7. `<adjust-panner>` (flat, only when no pan keyframes)
8. Connected clips (lane ≠ 0, nested inside)

### 20.10 Attributes Omitted in Strict Mode

The strict writer omits several attributes and elements that are present in the rich `.uspxml` format:

- `us:*` vendor namespace attributes (speed keyframes, clip metadata)
- `fcpxml_unknown_*` passthrough fragments
- Sequence markers (not supported by FCP's `<spine>` import)
- Asset `duration` (optional per DTD; omitting avoids GStreamer duration rounding exceeding actual media length)
