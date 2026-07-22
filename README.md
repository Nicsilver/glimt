# ⚡ Glimt

Lightning-fast screenshots for Windows.

<p align="center">
  <a href="https://github.com/Nicsilver/glimt/releases/latest"><img src="https://img.shields.io/github/v/release/Nicsilver/glimt?color=FFC53D" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/license-AGPL--3.0-blue" alt="License">
</p>

Glimt sits in your tray and waits for PrtSc. The instant you press it, the screen freezes on every monitor. Drag to select an area, annotate it if you want, then Ctrl+C puts the PNG on your clipboard or Ctrl+S drops it in your Pictures folder. No dialogs, no delay, no cloud.

## Features

- Instant capture: the app is pre-warmed, so the overlay appears the moment you press PrtSc
- Multi-monitor freeze with per-monitor DPI awareness
- Pixel-precise selection: zoom loupe while dragging, size badge, resize handles, arrow-key nudge (Shift for 10 px)
- Annotations: pen, line, arrow, rectangle and text in five colors, with Ctrl+Z undo
- Ctrl+C copies to clipboard, Ctrl+S saves straight to `Pictures\Glimt` with a timestamped name
- Single portable exe, starts with Windows (toggle in the tray menu)
- Planned: gif and mp4 capture

## Install

Grab `glimt.exe` from [Releases](https://github.com/Nicsilver/glimt/releases) and run it. That's it. The exe is unsigned, so SmartScreen will warn once.

## Shortcuts

| Key | Action |
| --- | --- |
| PrtSc | Freeze the screen and start selecting |
| Drag | Select an area (with zoom loupe) |
| Arrow keys | Nudge the selection 1 px (Shift: 10 px) |
| Ctrl+C | Copy the selection to the clipboard |
| Ctrl+S | Save to `Pictures\Glimt` |
| Ctrl+Z | Undo last annotation |
| Esc | Cancel |

<!-- TODO: add screenshots/ with a capture of the overlay + toolbar -->

## Building

```
cargo build --release
```

The exe lands in `target\release\glimt.exe`.

## Releasing

Push a tag `v*` and GitHub Actions builds the zip and attaches it to a release.

## License

AGPL-3.0
