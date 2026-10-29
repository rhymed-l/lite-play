# lite-play

A small desktop audio player for listening to long series of audio files, with optional
subtitles that are generated on your own computer.

Point it at a folder and it becomes the playlist. It remembers where you stopped in every file,
keeps playing from the system tray, and can be controlled from anywhere with global hotkeys.
Subtitles are an optional add-on: nothing is uploaded, and nothing is installed until you ask.

Built with [Tauri 2](https://tauri.app/) (Rust + a small TypeScript front end).

> **Status:** early (0.1.0). Developed and tested on Windows 11 only. The player is written to be
> cross-platform, but the subtitle feature is Windows-only for now. See [Known issues](#known-issues).

## Features

**Player**

- Folder as playlist: only the top level of the folder is scanned, sorted in natural order
  (`2.mp3` before `10.mp3`). The list updates when files are added or removed.
- Formats: mp3, m4a (AAC and ALAC), flac, ogg, wav.
- Remembers the position of every file, and the last file you played. Can be switched off; turning
  it off never deletes what was remembered.
- Play modes: in order, repeat one, shuffle (shuffle never reorders the list and does not repeat
  recent files). Forward or reverse order.
- Closing the window only hides it; playback continues from the tray. Quit from the tray menu.
- Global hotkeys, which work while the window is hidden:

  | Keys | Short press | Hold |
  |---|---|---|
  | `Ctrl+Alt+Space` | play / pause | |
  | `Ctrl+Alt+→` | next track | fast-forward |
  | `Ctrl+Alt+←` | previous track | rewind |

- Opening an audio file with lite-play (or passing it on the command line) plays it and uses its
  folder as the playlist; an already running instance is reused.

**Subtitles (optional, Windows only)**

- Generated from the audio, offline, on your computer, and written next to each audio file as a
  UTF-8 `.srt` with the same name. A file that already has a `.srt` is never touched or redone.
- A "subtitle" tab shows the full text of the current file, highlights the line being spoken, and
  jumps to a line when you click it.
- A floating desktop subtitle: one line, transparent background, draggable, can be locked
  (clicks pass through it), can sit on top of the taskbar. Font size and colour are adjustable.
  Text wider than the window scrolls in time with the audio.
- Order of work: the file you are on first, then the ones after it, then the ones before it.
- Speed levels, so it can run quietly in the background or use the whole machine:

  | Level | Share of CPU cores | Notes |
  |---|---|---|
  | 轻量 (light), default | about a quarter | barely noticeable |
  | 均衡 (balanced) | about half | |
  | 全速 (full) | all of them, at most 16 jobs | |

  The number of files made at once is also capped by free memory (about 450 MB per file, with
  3 GB kept free), re-checked before every new file. Running files are never stopped for that
  reason.

## How subtitles work, and what gets downloaded

lite-play does not ship a speech recognizer. The first time you switch subtitles on it asks
before downloading about 180 MB from GitHub, then installs it under its own data folder:

| Component | Source |
|---|---|
| Recognizer program | latest [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) release (Windows, shared, no TTS) |
| Speech model | newest int8 **SenseVoice** package in sherpa-onnx's `asr-models` release |
| Voice activity detection | `silero_vad.onnx` from the same `asr-models` release |

- Every download is checked against the SHA-256 that GitHub publishes for it. A file without a
  published digest is refused.
- Downloads are only accepted from `github.com/k2-fsa/sherpa-onnx/releases/download/`.
- A new install is built next to the current one and only becomes active after a self-test
  recognises a sample sentence. If anything fails, the old install stays as it was.
- On start, lite-play looks once for a newer version (only when subtitles are on and installed).
  It only tells you; it never updates by itself.
- The only network traffic is those version lookups and downloads. No audio, file names or other
  data leave your computer.
- "Uninstall" removes the add-on; the `.srt` files you already have are kept.

The model currently chosen is the newest one available. At the time of writing that version
produces text **without punctuation**, so each subtitle line is one stretch of speech.

## Install

There are no prebuilt releases yet. Build it yourself, below.

## Build from source

Requirements (Windows):

- [Rust](https://rustup.rs/) (stable, MSVC toolchain) and the Visual Studio C++ build tools
- [Node.js](https://nodejs.org/) 24 or newer and [pnpm](https://pnpm.io/)
- The WebView2 runtime (already present on Windows 11)
- Tauri's other [prerequisites](https://tauri.app/start/prerequisites/)

```powershell
pnpm install
pnpm tauri dev                 # run in development
pnpm tauri build --no-bundle   # release exe: src-tauri/target/release/lite-play.exe
pnpm tauri build               # also builds the NSIS installer
```

The versions of the Rust crates are pinned exactly in `src-tauri/Cargo.toml`.

## Tests

```powershell
cargo test --manifest-path src-tauri/Cargo.toml   # Rust: playback rules, subtitles, installer
pnpm test                                         # TypeScript: subtitle scrolling, speed hints
pnpm build                                        # type-check and bundle the front end
```

A few tests are marked `#[ignore]` because they need the network, an audio device, an installed
add-on or a real recording. Run them on purpose with `-- --ignored`, for example:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml live_install -- --ignored --nocapture
```

Several of them take their input from environment variables (`LITE_PLAY_ADDON_ROOT`,
`LITE_PLAY_EPISODE`, `LITE_PLAY_ADDON_DIR`); the variable names are in the test source.

## Where your data is

Everything lives in `%APPDATA%\com.liteplay.app\`:

| File | Content |
|---|---|
| `memory.json` | folders, per-file positions, mode, volume |
| `subtitle-settings.json` | subtitle switches, font size, colours, speed level, window position |
| `subtitle-addon\` | the downloaded recognizer, model and the list of subtitles lite-play wrote |

Subtitle files are written next to the audio, not in this folder.

## Known issues

These are real, not hypothetical:

- **Buttons and tray menu stopped responding once** during a long session while the global hotkeys
  kept working. It did not happen again after a restart and I have not found the cause.
- **Subtitle accuracy has not been measured.** It was compared by eye on a few sentences, with no
  reference text. Expect wrong characters, especially for names and rare words.
- Whether the desktop subtitle always stays above a clicked taskbar has been checked on one
  machine (Windows 11, two monitors) only.
- Only Windows has been tested. macOS and Linux have not been built or run, and the subtitle
  feature is disabled there.
- No GPU acceleration: the recognizer package used is CPU-only.

## Licence

[MIT](LICENSE). The downloaded components have their own licences: sherpa-onnx is Apache-2.0 and
the SenseVoice project is MIT. The model package's own licence file only points to the
[FunASR](https://github.com/modelscope/FunASR#license) licence; check it before redistributing the
model.
