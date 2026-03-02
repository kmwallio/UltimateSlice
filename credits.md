---
layout: default
title: Credits
permalink: /credits/
---

<div class="hero" style="padding-bottom: 40px;">
  <img src="{{ "/assets/images/io.github.kmwallio.ultimateslice.svg" | relative_url }}" alt="UltimateSlice Icon" style="width: 80px; height: 80px; margin-bottom: 20px;">
  <h1>Credits</h1>
  <p>The components that make UltimateSlice possible.</p>
</div>

<div class="wrapper" markdown="1" style="max-width: 900px; margin: 0 auto; padding: 40px 20px;">

UltimateSlice is built on the shoulders of giants. We are proud to use and support the following open-source projects.

## Project License

UltimateSlice is licensed under the **[GNU GPL v3.0 or later](https://www.gnu.org/licenses/gpl-3.0.html)**.  
This license ensures that the editor remains free and open-source, and allows us to distribute a high-quality Flatpak bundle including x264 and FFmpeg with GPL-licensed components.

## Core Technologies

- **[Rust](https://www.rust-lang.org/)**: The programming language that provides the performance and safety foundations for UltimateSlice. (MIT/Apache 2.0)
- **[GTK4](https://www.gtk.org/)**: The cross-platform widget toolkit used for our user interface. (LGPL 2.1+)
- **[GStreamer](https://gstreamer.freedesktop.org/)**: The powerful multimedia framework that handles all of our playback and rendering. (LGPL 2.1+)
- **[FFmpeg](https://ffmpeg.org/)**: Used for high-quality video and audio encoding on export. (LGPL/GPL)

## Rust Libraries (Crates)

We rely on many excellent crates from the Rust ecosystem:

- **[gtk4-rs](https://gtk-rs.org/)**: Safe Rust bindings for GTK4. (MIT)
- **[gstreamer-rs](https://gitlab.freedesktop.org/gstreamer/gstreamer-rs)**: Safe Rust bindings for GStreamer. (MIT/Apache 2.0)
- **[Serde](https://serde.rs/)**: A framework for serializing and deserializing Rust data structures efficiently and generically. (MIT/Apache 2.0)
- **[quick-xml](https://github.com/tafia/quick-xml)**: High-performance XML pull reader/writer used for FCPXML support. (MIT)
- **[Anyhow](https://github.com/dtolnay/anyhow)** & **[Thiserror](https://github.com/dtolnay/thiserror)**: Flexible error handling for Rust. (MIT/Apache 2.0)
- **[Log](https://github.com/rust-lang/log)** & **[Env_logger](https://github.com/rust-lang/env_logger)**: Logging abstractions and implementation. (MIT/Apache 2.0)
- **[UUID](https://github.com/uuid-rs/uuid)**: Generate and parse UUIDs. (MIT/Apache 2.0)

## Other Components

- **[Adwaita](https://gnome.pages.gitlab.gnome.org/libadwaita/)**: Inspiration for our dark-themed UI following the GNOME Human Interface Guidelines.
- **[Model Context Protocol (MCP)](https://modelcontextprotocol.io/)**: The protocol used to enable AI collaboration within the editor.

---

### License Note
This project attributes these components in accordance with their respective licenses. If you believe an attribution is missing or incorrect, please [open an issue on GitHub](https://github.com/kmwallio/UltimateSlice/issues).

</div>
