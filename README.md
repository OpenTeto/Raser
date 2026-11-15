# ⚡ Raser — Ultra-Lightweight Native Music Player

<div align="center">

  <img src="ui/assets/icon-512x512.png" width="128" height="128" alt="Raser Logo" />

  <h3>The Fastest, Lightest & Most Elegant Music Streaming Experience</h3>

  [![Release](https://img.shields.io/github/v/release/tetoverse/Raser?color=6366f1&style=for-the-badge)](https://github.com/tetoverse/Raser/releases)
  [![Rust](https://img.shields.io/badge/Rust-1.90%2B-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
  [![Tauri](https://img.shields.io/badge/Tauri-v2-24c8db?style=for-the-badge&logo=tauri)](https://tauri.app/)
  [![Platform](https://img.shields.io/badge/Platform-Windows%2010%2F11-0078d4?style=for-the-badge&logo=windows)](https://github.com/tetoverse/Raser/releases)
  [![License](https://img.shields.io/badge/License-MIT-green?style=for-the-badge)](LICENSE)

  <p align="center">
    <b>Raser</b> is an ultra-modern, blazing-fast native music desktop player powered by <b>Tauri v2 + Rust</b> with a stunning <b>Material You 3 (M3)</b> design system. It combines YouTube Music & SoundCloud streaming, rich Discord Presence, synced lyrics, and native hardware performance while consuming a fraction of the memory of traditional Electron players.
  </p>

</div>

---

## 🌟 Why Raser? (Performance Benchmarks)

Traditional music players (Spotify, YouTube Music Desktop, Electron-based players) consume massive system resources even when idling in the background. **Raser was engineered from the ground up to solve this.**

| Metric | Raser v1.x.x (Rust Audio Core) | Electron / Spotify Player | Difference |
| :--- | :---: | :---: | :---: |
| **Idle RAM (Background / Tray)** | **~35 - 55 MB** | 450 - 750 MB | **~10x Lighter** |
| **Active Playback RAM** | **~75 - 110 MB** | 600 - 950 MB | **~8x Lighter** |
| **Installer File Size** | **~8.0 MB** | 120 - 180 MB | **~15x Smaller** |
| **Cold Start Time** | **< 0.6 seconds** | 3.5 - 6.0 seconds | **Instant** |
| **Cold Stream Start (192 KB)** | **~1.3 - 1.8 seconds** | 4.0 - 8.0+ seconds | **~3-4x Faster** |
| **Audio Engine** | **Native Rust (`rodio` + WASAPI)** | Web Audio API / Chromium | **Zero Audio Lag** |

---

## ✨ Features

### 🎧 Pure Rust Audio Engine (Stage 3 Architecture)
- **Zero Web Audio Overhead:** Audio decoding and streaming are handled natively in Rust via `rodio` and `symphonia`, bypassing Chromium's memory-heavy `AudioService` process.
- **Lock-Free FFT Visualizer:** Audio frequencies are analyzed on a dedicated audio thread using `rustfft` and an `rtrb` lock-free ring buffer, providing 60 FPS buttery-smooth canvas visualizer waves with zero UI lag.
- **Background Playback & Instant Sleep:** When minimized or hidden to the system tray, UI rendering loops are completely frozen, V8 memory is scrubbed via `--expose-gc`, and Windows working sets are trimmed.
- **Gapless & Progressive Caching:** Remote streams are buffered progressively in-memory and persisted to local SSD cache (`~6 ms` warm read) for instant zero-latency seeking.
- **150ms Visibility Prewarm & Dual-Tier Streaming:** Playlist tracks visible in the viewport for 150ms are automatically pre-resolved and persistently cached, while cold streams start in **~1.3–1.8s** via Dual-Tier chunked streaming.

### 🎨 Material You 3 (M3) Design
- **Dynamic Palette Extraction:** UI accents automatically adapt to the dominant colors of the currently playing album art.
- **Morphing Album Art:** Fluid canvas transitions between tracks with glassmorphism and ambient lighting effects.
- **Synchronized Karaoke Lyrics:** Word-by-word and line-by-line synchronized lyrics with real-time gradient follow.
- **TV Karaoke Mode:** Fullscreen karaoke view with live dynamic visual effects.

### 🪟 Deep Windows Integration
- **Taskbar Thumbnail Toolbar:** Hover over Raser in the Windows Taskbar to instantly access Prev, Play/Pause, and Next buttons via native Windows `ITaskbarList3` APIs.
- **System Tray Minimization:** Close button hides Raser to the system tray with full background playback support and global hotkeys.
- **Discord Rich Presence (RPC):** Native rich presence shows current song, artist name ("Listening to [Artist]"), dynamic time elapsed, and album art on your Discord profile.
- **Windows Media Controls (SMTC):** Supports hardware keyboard media keys (Play, Pause, Stop, Next, Previous).

---

## 📥 Download & Installation

Head over to the **[Latest Release](https://github.com/tetoverse/Raser/releases/latest)** to download the installer:

- **Standard Installer (Recommended):** [`Raser_1.x.x_x64-setup.exe`](https://github.com/tetoverse/Raser/releases/latest) (NSIS Installer)
- **Enterprise / Windows Installer:** [`Raser_1.x.x_x64_en-US.msi`](https://github.com/tetoverse/Raser/releases/latest) (MSI Package)

> **Silent Installation (CLI):**
> ```cmd
> Raser_1.x.x_x64-setup.exe /S
> ```

---

## ⌨️ Global Keyboard Shortcuts

| Shortcut | Action |
| :--- | :--- |
| `Space` | Play / Pause |
| `MediaTrackNext` / `Ctrl + Right` | Next Track |
| `MediaTrackPrevious` / `Ctrl + Left` | Previous Track |
| `MediaStop` | Stop Playback |
| `Up` / `Down` | Volume Up / Down (5%) |
| `Ctrl + F` | Quick Search Focus |
| `Esc` | Close Modals / Clear Track Selection |

---

## 🛠️ Building From Source

### Prerequisites
1. **Rust & Cargo:** Install via [rustup.rs](https://rustup.rs/) (Rust 1.90+ required).
2. **Node.js:** v18+ with `npm` / `npx`.
3. **C++ Build Tools:** Visual Studio C++ Build Tools with Windows 10/11 SDK.

### Build Steps

```bash
# 1. Clone repository
git clone https://github.com/tetoverse/Raser.git
cd Raser

# 2. Check Rust dependencies
cd src-tauri
cargo check

# 3. Build Production Installer (EXE & MSI)
cd ..
npx @tauri-apps/cli build
```

The resulting executables and installers will be generated under:
`src-tauri/target/release/bundle/nsis/Raser_1.x.x_x64-setup.exe`
`src-tauri/target/release/bundle/msi/Raser_1.x.x_x64_en-US.msi`

---

## 📂 Project Architecture

```
Raser/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs            # Application entry point
│   │   ├── lib.rs             # Tauri command bridge & background monitors
│   │   ├── audio_core.rs      # Native Rust Audio Engine (rodio, symphonia, FFT)
│   │   ├── discord.rs         # Discord Rich Presence IPC integration
│   │   ├── taskbar.rs         # Windows Taskbar thumbnail buttons (ITaskbarList3)
│   │   ├── storage.rs         # Offline encrypted & atomic JSON storage
│   │   └── updater.rs         # Auto-updater management
│   ├── Cargo.toml             # Rust dependencies & compiler profile optimizations
│   └── tauri.conf.json        # Tauri configuration & Chromium V8 flags
└── ui/                        # Material You frontend (HTML5/CSS3/Vanilla JS)
    ├── index.html
    ├── styles.css
    ├── app.js
    └── tauri-bridge.js        # IPC binding between UI and Rust Audio Core
```

---

## 📄 License

This project is licensed under the MIT License — see the [LICENSE](LICENSE) file for details.

Developed with ❤️ by **[tetoverse](https://github.com/tetoverse)**.
