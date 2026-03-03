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
*   **`version`**: The schema version. Version 1.10+ is required for many modern features like HDR and Cinematic mode.
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


---

## 4. Metadata, Markers (Locators) & Captions
These elements provide context and organization. In FCPXML, "locators" are implemented via various marker tags.

*   **`<marker>`**: A point-of-interest locator. Use it for general notes.
*   **`<chapter-marker>`**: A locator that defines a "jump point" for DVD/Blu-ray or video players (YouTube chapters).
*   **`<analysis-marker>`**: Created automatically by FCP for things like "Face Detection" or "Shaky Video."

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
*   **`<adjust-conform>`**: 
    *   Determines how to fit a clip into a sequence when their formats (resolutions) don't match.
    *   **`type="fit"`**: Scales the clip to fit entirely inside the frame (letterboxing if needed).
    *   **`type="fill"`**: Scales the clip to fill the entire frame (cropping if needed).
    *   **`type="none"`**: Keeps the clip at its original pixel size.
*   **`<adjust-crop>`**: Trims the edges of the video.

### **Audio Attributes**
*   **`amount` (Volume)**: Measured in decibels (dB). `0dB` is the original volume. `-inf` is muted.
*   **`fadeIn/fadeOut`**: Automatically smooths the start or end of the sound.

### **Timing & Keyframes**
Keyframes within an adjustment tag use the `time` attribute, which is a rational number. However, the "effective" frame they land on is determined by the `frameDuration` of the element's format.

### **Transformation Math (Developer Guide)**
When building a renderer, use these formulas to convert FCPXML values into standard graphics engine coordinates. FCPXML uses **normalized coordinates based on frame height**.

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

### **Developer Example: Animated Opacity Fade**
*This clip starts invisible and takes 2 seconds to fade in to full 100% visibility.*

```xml
<adjust-transform>
    <param name="opacity">
        <keyframeAnimation>
            <keyframe time="0s" value="0.0"/>
            <keyframe time="2s" value="1.0"/>
        </keyframeAnimation>
    </param>
</adjust-transform>
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
4.  **Coordinate Flip**: Most coding frameworks use `(0,0)` as the top-left corner. FCPXML uses `(0,0)` as the **center**. You will need to offset your coordinates to match.
