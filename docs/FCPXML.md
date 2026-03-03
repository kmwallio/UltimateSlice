# FCPXML Reference & Parsing Guide

This document provides a technical overview of the FCPXML (Final Cut Pro XML) format, its structure, key elements, and how to accurately parse its attributes. This guide is designed to be a functional blueprint for building an FCPXML-compatible video editor.

---

## 1. Overview
FCPXML is an XML-based interchange format used by Final Cut Pro to describe libraries, events, projects, and their contained media and timelines. 

**Key Concept: Definition vs. Usage**
FCPXML separates the **definition** of a resource (like a high-res video file or a specific frame rate) from its **usage** (how many seconds of that file appear in your edit). This allows you to change the source file once in the "Resources" section, and every instance of it in your timeline will update automatically.

---

## 2. Basic Structure & Projects
FCPXML follows a strictly hierarchical structure, mirroring the organization of a real-world video project.

### **Attribute Meanings**
*   **`version`**: The schema version (e.g., `1.10`, `1.11`, `1.12`, `1.13`). Version 1.10+ is required for many modern features like HDR and Cinematic mode. Version 1.13 corresponds to Final Cut Pro 11.
*   **`format`**: A unique ID referencing a `<format>` resource. This defines the "canvas" (resolution) and "heartbeat" (frame rate) of the element.
*   **`tcStart`**: The starting timecode of the timeline. Usually `3600s` (01:00:00:00).
*   **`tcFormat`**: Determines if the clock skips frames to stay in sync with real-time (`DF` for Drop Frame) or counts every single frame (`NDF` for Non-Drop Frame).

### **The `<resources>` Dictionary**
FCPXML uses a central repository for all "heavy" objects. This avoids duplicating data when the same file or effect is used multiple times in a timeline.

*   **`<format>`**: Defines the canvas (resolution and frame rate). See above for details.
*   **`<asset>`**: Represents a physical file on disk (video, audio, or image).
    *   **`src`**: The file URL (e.g., `file:///media/clip.mov`).
    *   **`start` / `duration`**: The intrinsic bounds of the file.
    *   **`hasVideo` / `hasAudio`**: Boolean flags (`1` or `0`).
*   **`<media>`**: Represents "synthetic" or "nested" media, such as Multicam clips or Compound clips. It contains its own internal timeline.
*   **`<effect>`**: Defines a visual or audio filter, transition, or generator.
    *   **`uid`**: A unique string identifying the specific plugin (e.g., `.../Video Filters/Color/Color Board`).

### **Developer Example: Resource Interaction**
*This example shows how a sequence (r1) uses an asset (r2) and applies an effect (r3).*

```xml
<resources>
    <format id="r1" frameDuration="100/2400s" width="1920" height="1080"/>
    <asset id="r2" name="Interview" src="file:///media/clip.mov" start="0s" duration="100s"/>
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
*   **`<mc-angle>`**: A container for a single camera's footage. Each angle has a unique `angleID`.
*   **`<mc-clip>`**: The usage tag in the timeline that references a multicam resource.
    *   **`videoAngleID`**: The ID of the angle currently being shown.
    *   **`audioAngleID`**: The ID of the angle currently being heard.
*   **`<mc-source>`**: Used inside an `<mc-clip>` to create "Angle Cuts." It overrides the default angle for a specific segment of the clip.

### **Developer Example: Multicam Setup & Angle Switching**
*In this example, we have two cameras. The clip starts by showing Angle 1, then mid-clip, we "cut" to Angle 2 for the video while keeping the audio from Angle 1.*

```xml
<resources>
    <media id="r10" name="Interview Multicam">
        <multicam>
            <mc-angle name="Angle 1" angleID="cam1">
                <asset-clip ref="r2" offset="0s" duration="100s"/>
            </mc-angle>
            <mc-angle name="Angle 2" angleID="cam2">
                <asset-clip ref="r3" offset="0s" duration="100s"/>
            </mc-angle>
        </multicam>
    </media>
</resources>

<!-- The main instance uses cam1 for both video and audio by default -->
<mc-clip ref="r10" offset="3600s" duration="10s" videoAngleID="cam1" audioAngleID="cam1">
    <!-- At this point in the timeline, we switch the video to cam2 -->
    <!-- The 'usage' attribute can be 'video' or 'audio' -->
    <mc-source angleID="cam2" usage="video" start="5s" duration="5s"/>
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
    *   Can contain: `<audio>`, `<video>`, `<title>`, `<ref-clip>`, `<asset-clip>`, `<clip>`, `<sync-clip>`, `<mc-clip>`.

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
    *   **`mode`**: Blend mode (e.g., `Subtract`, `Multiply`). If omitted, uses normal compositing.

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
Keyframes within an adjustment tag use the `time` attribute, which is a rational number. However, the "effective" frame they land on is determined by the `frameDuration` of the element's format.

Each `<keyframe>` element also supports:
*   **`interp`**: Interpolation mode — `linear` | `ease` | `easeIn` | `easeOut`. Default: `linear`.
*   **`curve`**: Curve shape — `linear` | `smooth`. Default: `smooth`.

### **Transformation Math (Developer Guide)**

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
    *   **`scaleEnabled`**: Whether rate conforming is active (`0` | `1`).
    *   **`srcFrameRate`**: The source frame rate to conform from.

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
    *   **`version`**: The schema version (e.g., `1.10`, `1.11`, `1.12`, `1.13`).
*   **`<library>`**: Represents an FCP library.
    *   **`location`**: The file URL to the library on disk.
*   **`<event>`**: A container for clips and projects.
    *   **`name`**: The display name of the event.
*   **`<project>`**: Represents a Final Cut Pro project (timeline).
    *   **`name`**: The display name of the project.

### **Resource Tags (Inside `<resources>`)**
*   **`<format>`**: Defines resolution and frame rate.
    *   **`id`**: Unique resource ID.
    *   **`name`**: Symbolic name (e.g., `FFVideoFormat1080p24`).
    *   **`frameDuration`**: Frame time in rational seconds (e.g., `100/2400s`).
    *   **`width` / `height`**: Frame dimensions in pixels.
    *   **`colorSpace`**: The color profile (e.g., `Rec. 709`, `Rec. 2020`).
*   **`<asset>`**: References a physical media file.
    *   **`id`**: Unique resource ID.
    *   **`src`**: File URL.
    *   **`start` / `duration`**: Intrinsic bounds of the file.
    *   **`hasVideo` / `hasAudio`**: Boolean flags (`1` or `0`).
    *   **`uid`**: A unique ID used for media linking.
*   **`<media>`**: Represents synthetic or nested media (Compound/Multicam).
    *   **`id`**: Unique resource ID.
    *   **`name`**: Display name.
*   **`<effect>`**: Defines a plugin or filter.
    *   **`id`**: Unique resource ID.
    *   **`uid`**: The system path/ID for the effect plugin.

### **Timeline & Story Elements**
*   **`<sequence>`**: The main timeline container.
    *   **`format`**: Reference to a `<format>` ID.
    *   **`duration`**: Total length of the sequence.
    *   **`tcStart`**: Starting timecode (usually `3600s`).
    *   **`tcFormat`**: Timecode format (`DF` or `NDF`).
*   **`<spine>`**: The primary "storyline" of the timeline.
*   **`<asset-clip>`**: A clip referencing an `<asset>`.
    *   **`ref`**: Reference to an `<asset>` ID.
    *   **`offset`**: Start time relative to parent.
    *   **`start` / `duration`**: Range within the asset.
    *   **`lane`**: Vertical position (default `0`).
    *   **`role`**: Media role (e.g., `dialogue`, `titles`).
*   **`<clip>`**: A generic clip element (distinct from `<asset-clip>`).
    *   Supports `format`, `audioStart` / `audioDuration` (for split edits), `tcStart` / `tcFormat`.
    *   Can contain nested spines, captions, markers, and filters.
*   **`<video>`**: A video-only clip referencing an asset.
    *   **`ref`**: Reference to an `<asset>` ID.
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
    *   Contains `<sync-source>` children mapping source streams.
*   **`<audition>`**: Container for audition alternatives.
    *   First child is the active "pick"; subsequent children are alternatives.
    *   Can contain: `<audio>`, `<video>`, `<title>`, `<ref-clip>`, `<asset-clip>`, `<clip>`, `<sync-clip>`, `<mc-clip>`.
*   **`<mc-clip>`**: A clip referencing a `<multicam>` media resource.
    *   **`ref`**: Reference to a `<media>` ID.
    *   **`videoAngleID` / `audioAngleID`**: Active angle IDs.
*   **`<ref-clip>`**: A clip referencing a `<media>` resource (Compound clip).
*   **`<gap>`**: A placeholder for empty space.
    *   **`duration` / `offset`**: Position and length.
*   **`<transition>`**: A transition effect between two clips.
    *   **`name`**: Display name of the transition.
    *   **`offset`**: Position on the timeline.
    *   **`duration`**: Length of the transition.
    *   Contains optional `<filter-video>`, `<filter-audio>`, markers, and metadata.
*   **`<title>`**: A text overlay generator.
*   **`<caption>`**: A closed caption element.
    *   **`role`**: Caption role assignment.
    *   Standard clip attributes (`offset`, `duration`, etc.).
    *   Contains `<text>`, `<text-style-def>`.

### **Metadata & Locators**
*   **`<marker>`**: A point-of-interest locator.
    *   **`start`**: Position relative to the clip's start.
    *   **`value`**: Note or name.
*   **`<chapter-marker>`**: A locator for chapter markers.
    *   **`posterOffset`**: Thumbnail frame position.
*   **`<keyword>`**: A tag applied to a time range.
    *   **`start` / `duration`**: The range covered by the tag.
*   **`<metadata>`**: Container for key-value metadata.
    *   **`<md>`**: A metadata entry with `key` and `value` attributes.
*   **`<note>`**: A simple text element for attaching notes to clips or other elements.

### **Adjustments & Effects**
*   **`<adjust-transform>`**: Spatial transformations.
    *   **`position`**: XY translation (percentage of height).
    *   **`scale`**: XY multiplier.
    *   **`rotation`**: Rotation in degrees (positive is CCW).
    *   **`anchor`**: Center of transformation (percentage of height).
    *   **`tracking`**: IDREF linking to an object tracker.
*   **`<adjust-blend>`**: Opacity and compositing blend mode.
    *   **`amount`**: Opacity from `0.0` to `1.0` (default: `1.0`).
    *   **`mode`**: Blend mode (e.g., `Subtract`, `Multiply`). Omit for normal compositing.
*   **`<adjust-conform>`**: Resolution scaling rules.
    *   **`type`**: `fit`, `fill`, or `none`.
*   **`<adjust-crop>`**: Trims the edges of the video.
    *   **`mode`**: Crop mode (`trim`, `crop`, `pan`).
*   **`<adjust-volume>`**: Audio level controls.
    *   **`amount`**: Volume in dB.
*   **`<filter-video>` / `<filter-audio>`**: Usage tags for effects.
    *   **`ref`**: Reference to an `<effect>` ID.

### **Audio Channel Configuration**
*   **`<audio-channel-source>`**: Maps source audio channels to output channels.
    *   **`srcCh`**: Source channel(s) (e.g., `"1, 2"`).
    *   **`outCh`**: Output channel assignment (e.g., `"L, R"`).
    *   **`role`**: Audio role for these channels.
    *   **`enabled`** / **`active`**: Boolean flags (`1` or `0`).
*   **`<audio-role-source>`**: Manages role-based audio mixing.
    *   **`role`**: The audio role (e.g., `dialogue.dialogue-1`).
    *   **`enabled`** / **`active`**: Boolean flags (`1` or `0`).

### **Speed & Retime**
*   **`<timeMap>`**: Container for speed mapping points.
    *   **`frameSampling`**: `floor` | `nearest-neighbor` | `frame-blending` | `optical-flow-classic` | `optical-flow` | `optical-flow-frc`.
    *   **`preservesPitch`**: `0` | `1` (default: `1`).
*   **`<timept>`**: A time mapping point.
    *   **`time`**: Output (timeline) time.
    *   **`value`**: Corresponding source time.
    *   **`interp`**: `smooth2` | `linear` | `smooth` (default: `smooth2`).
*   **`<conform-rate>`**: Frame rate conforming.
    *   **`scaleEnabled`**: `0` | `1`.
    *   **`srcFrameRate`**: Source frame rate to conform from.
