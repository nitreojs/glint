# Glint

Glint is a Windows tray app for screenshots, screen recording, and quick video edits.

**[Download Glint for Windows](https://github.com/nitreojs/glint/releases/latest)** · [Afterglow replay recorder](#afterglow)

Run the downloaded `.exe` to start Glint in the system tray. It checks for updates automatically.

<p align="center"><picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/hero-light.png"><img src="docs/readme/hero.png" alt="glint" width="880"></picture></p>

## Capture and annotate

Press **Ctrl+Print Screen** and drag to capture a region. Mark it up with arrows, boxes, numbered steps, or text. Blur or pixelate anything you don't want to share.

- **Reframe without redrawing.** Annotate anywhere on the frozen screen and resize the selection. Only the selected area is copied or saved. Enable the magnifier for precise resizing.
- **Make the toolbar yours.** Hide tools, reorder them, group them, or assign your own shortcuts.
- **Pin a reference.** Grab part of the screen or paste a picture with **Ctrl+V**, then pin it above other windows. Drag a corner to resize it; hold **Shift** to keep its proportions.

<img src="docs/readme/1-screenshot.avif" alt="Capturing and annotating a screenshot in Glint" width="880">

<details>
<summary>See how pinned captures work</summary>

<img src="docs/readme/2-grab.avif" alt="Pinning and enlarging a region of the screen" width="440">

</details>

## Record and edit video

Press **Ctrl+Alt+Print Screen** and pick a region or window. Glint records on the GPU with mic and system audio, and can show clicks and keystrokes.

In the editor, trim or crop the video, mute individual tracks, or export to a target file size. Use **← / →** to move one frame at a time.

<img src="docs/readme/3-record.avif" alt="Recording a region and trimming the clip in Glint" width="880">

<details>
<summary>Timeline shortcuts and editing tips</summary>

- **Trim:** Drag either trim edge toward the playhead to snap to it. Hold **Ctrl** to bypass snapping.
- **Split and remove:** Press **S** at both ends of a section, then right-click the piece in the thumbnail strip to remove or restore it.
- **Change speed:** **Shift+drag** an edge of a kept piece to stretch or shorten playback without trimming the source. The other edge stays put, and the current speed and duration appear as you drag. Audio pitch follows the speed.
- **Target a shared edge:** Park the playhead in the piece you want, or drag from its side of the split.
- **Reset or merge:** Double-click a retimed piece to reset it to **1×**; **Ctrl+Z** restores its previous speed. Right-click a split to merge two kept pieces at the same speed.
- **Preview faster:** Glint and Afterglow collect waveform peaks and a thumbnail overview while recording. For other videos, the editor draws waveforms as it loads audio, then caches them with the filmstrip for later opens. The cache stays on your PC and doesn't change the video.

</details>

<details>
<summary>Exporting, sharing, and keeping originals</summary>

- **Lossless cuts:** Glint shows when it can export without re-encoding. When combining sound, tracks that end early fall silent while the clip continues.
- **Share a saved clip:** Drag its thumbnail or filename into a chat, or click it and paste with **Ctrl+V**. The card shrinks to a thumbnail beside **Save** after eight seconds, but keeps its drag and copy actions. Hover for file size or use the folder icon to show it in Explorer.
- **Keep or recycle the original:** In **Settings → Recording → After trimming**, choose **Keep both** (the default), **Keep trimmed only**, or **Ask each time**. Glint only recycles the original on editor close if a successful edited export still exists. Unsaved edits and later failed or cancelled exports keep the original.

</details>

## Find and share captures

The gallery brings your screenshots and clips into one place. Search by app, game, or day, hover over a clip to play it, or drag a capture straight into a chat.

- Under **Filter → Video edits**, choose **Untrimmed** for originals or **Trimmed** for exports.
- **Shift+click** a video to open it in a separate Glint editor without leaving the gallery.

<img src="docs/readme/4-gallery.avif" alt="Browsing screenshots and videos in Glint's gallery" width="880">

## Afterglow

Afterglow is a separate Windows tray app that keeps the last minute of your screen and audio. Press **Alt+F10** to save a replay, then open it in Glint to edit it. In **Deadlock**, automatic clips for kills are available in beta.

**[Find the latest Afterglow download](https://github.com/nitreojs/glint/releases?q=afterglow)**

<details>
<summary>Diagnostics and stalled replay saves</summary>

In either tray menu, open **Diagnostics** to see the current and previous run. Search for a clip name, filter by event type or level, or select an error to see the surrounding events. **Open raw log** opens the full file in Notepad.

Afterglow writes MP4s directly. Diagnostics reports whether a replay saved, was shortened by the memory cap, or timed out. If a save stalls, you can try another after 90 seconds. Any `.mp4.part` left behind stays separate from finished clips.

</details>

## Build from source

On Windows with Rust installed, run:

```sh
cargo build --release -p glint
```

The executable is at `target/release/glint.exe`. To build Afterglow, use `-p afterglow` instead.
