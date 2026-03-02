---
layout: default
title: Features
permalink: /features/
---

<div class="hero" style="padding-bottom: 40px;">
  <img src="{{ "/assets/images/io.github.kmwallio.ultimateslice.svg" | relative_url }}" alt="UltimateSlice Icon" style="width: 80px; height: 80px; margin-bottom: 20px;">
  <h1>Features</h1>
  <p>Powerful editing with an open heart.</p>
</div>

<div class="features-grid">
  <div class="feature-card">
    <h3>Professional Timeline</h3>
    <p>Multi-track rows, ripple-aware trimming, razor tool, and standard keyboard shortcuts (J/K/L, I/O).</p>
  </div>
  <div class="feature-card">
    <h3>Native Performance</h3>
    <p>Built with GTK4 and Rust for a smooth, high-frame-rate editing experience on any desktop.</p>
  </div>
  <div class="feature-card">
    <h3>FCPXML Interchange</h3>
    <p>Import and export projects from Final Cut Pro. Seamless transition for professional workflows.</p>
  </div>
  <div class="feature-card">
    <h3>MCP AI Server</h3>
    <p>Control the editor, list tracks, and add clips via AI agents using the Model Context Protocol.</p>
  </div>
</div>

<div class="wrapper" markdown="1" style="max-width: 1100px; margin: 0 auto; padding: 40px 20px;">

## Implemented Features

Our current stable development branch (`main`) features:

- **GTK4 UI Scaffold**: A modern, dark-themed interface following GNOME HIG.
- **Media Library Browser**: Import videos, audio, and images with automatic duration probing.
- **Source & Program Monitors**: Frame-accurate playback, scrubbing, volume control, and in/out markers.
- **Multi-track Timeline**: Flexible editing with support for video and audio tracks.
- **Visual Cues**: Filmstrip thumbnails for video and normalized waveforms for audio.
- **Advanced Trimming**: Ripple-aware trimming, razor tool, and standard keyboard shortcuts (J/K/L, I/O).
- **Real-time Effects**: Color correction, transforms, and transitions applied live.
- **Project Serialization**: Save and load projects in standard FCPXML format.
- **Background Rendering**: High-quality MP4/H.264 exports via GStreamer/FFmpeg.

## Planned Roadmap

Check out our [ROADMAP.md](https://github.com/kmwallio/UltimateSlice/blob/main/ROADMAP.md) for upcoming features:
- **Smart Script-to-Timeline**: AI-powered assembly from scripts.
- **Advanced Audio Tools**: Multichannel mixing and EQ.
- **More Transitions**: Wipe, slide, and custom GLSL transitions.
- **Plugin System**: Extend UltimateSlice with community-built tools.

</div>
