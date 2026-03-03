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
*   **`format`**: Defines the "canvas" of your video—its resolution (e.g., 1920x1080) and frame rate (e.g., 24fps).
*   **`tcStart`**: The starting timecode of the timeline. Usually `3600s` (which is 01:00:00:00).
*   **`tcFormat`**: Determines if the clock skips frames to stay in sync with real-time (`DF` for Drop Frame) or counts every single frame (`NDF` for Non-Drop Frame).

### **Developer Example: Basic Skeleton**
*This example shows a simple project with one 10-second clip sitting at the very beginning of a 1-hour timeline.*

```xml
<fcpxml version="1.11">
    <resources>
        <!-- Format: 1080p at 24 frames per second -->
        <format id="r1" name="FFVideoFormat1080p24" frameDuration="100/2400s" width="1920" height="1080"/>
        <!-- Asset: A physical file on your hard drive -->
        <asset id="r2" name="Interview" src="file:///media/clip.mov" start="0s" duration="100s" hasVideo="1" hasAudio="1"/>
    </resources>
    <library location="file:///path/to/library/">
        <event name="My Event">
            <project name="My Project">
                <sequence duration="10s" format="r1" tcStart="3600s" tcFormat="NDF" render-color-space="Rec. 709">
                    <spine>
                        <!-- Offset 3600s means this clip starts at 01:00:00:00 -->
                        <asset-clip ref="r2" offset="3600s" name="Clip 1" start="0s" duration="10s"/>
                    </spine>
                </sequence>
            </project>
        </event>
    </library>
</fcpxml>
```

---

## 3. Advanced Timeline Structures

### Multicam Editing
A Multicam clip is a "stack" of synchronized camera angles. In the editor, you don't see the stack; you only see the "active" angle you've chosen.

*   **`mc-angle`**: A container for a single camera's footage.
*   **`videoAngleID`**: Tells the player which camera's picture to show.
*   **`audioAngleID`**: Tells the player which camera's microphone to listen to.

### **Developer Example: Multicam Setup**
*In this example, we have two cameras (Angle 1 and Angle 2). The timeline starts by showing Angle 1, then mid-clip, we "cut" to Angle 2 for the video while keeping the audio from Angle 1.*

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

<mc-clip ref="r10" offset="3600s" duration="10s" videoAngleID="cam1" audioAngleID="cam1">
    <!-- This 'mc-source' is an "Angle Cut". It overrides the default cam1 video -->
    <mc-source angleID="cam2" usage="video"/>
</mc-clip>
```

### Secondary Storylines & Lanes
FCPXML uses a "Magnetic Timeline." The **Spine** is the main story. **Lanes** are for everything else (B-roll, titles, sound effects) that "hangs off" the main story.

*   **`lane="0"`**: The main story (Spine).
*   **`lane="1", "2"...`**: Clips stacked *on top* of the main story (overlays).
*   **`lane="-1", "-2"...`**: Clips stacked *underneath* (usually background music).

---

## 4. Metadata, Markers & Captions
These elements provide context and organization. They are always "anchored" to a clip, meaning if you move the clip, the markers and captions move with it.

*   **`marker`**: A simple bookmark with a name.
*   **`keyword`**: A tag applied to a *range* of time (e.g., "The 5 seconds where the subject laughs").
*   **`caption`**: Subtitles. They have their own `role` so you can toggle English vs. Spanish subtitles easily.

### **Developer Example: Organizing a Clip**
```xml
<asset-clip ref="r2" offset="0s" duration="10s">
    <metadata>
        <md key="com.apple.proapps.studio.scene" value="4"/>
    </metadata>
    
    <!-- This keyword only covers the first half of the clip -->
    <keyword start="0s" duration="5s" value="Action Scene"/>
    
    <!-- A standard marker at the 2.5s mark -->
    <marker start="2.5s" value="Actor sneezes"/>
</asset-clip>
```

---

## 5. Visual & Audio Adjustments
These define how a clip looks and sounds.

### **Video Attributes**
*   **`position`**: Where the clip is on screen. `0 0` is the center.
*   **`scale`**: How big it is. `1 1` is 100%. `2 2` is 200%.
*   **`rotation`**: Spinning the clip. `90` is a quarter-turn clockwise.
*   **`opacity`**: How see-through it is. `0.0` is invisible, `1.0` is solid.

### **Audio Attributes**
*   **`amount` (Volume)**: Measured in decibels (dB). `0dB` is the original volume. `-inf` is muted.
*   **`fadeIn/fadeOut`**: Automatically smooths the start or end of the sound.

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
