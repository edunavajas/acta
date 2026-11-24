[Español](README.md) · **English**

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/acta-horizontal-reversed.svg">
    <img src="docs/brand/acta-horizontal.svg" alt="acta" width="320">
  </picture>
</p>

<p align="center">
  <strong>You attend the meeting. acta writes the minutes.</strong><br>
  A meeting recorder for Mac that transcribes on your machine and hands back the summary, the decisions and <em>your</em> tasks.
</p>

<p align="center">
  macOS 14.2+ · Apple Silicon · Tauri v2 (Rust + React) · no account, no server of our own
</p>

<p align="center">
  <a href="https://github.com/edunavajas/acta/releases/download/v0.1.0/acta-demo.mp4">
    <img src="docs/brand/video-poster.jpg" alt="acta intro video: you and everyone else in the same meeting, each on their own track" width="860">
  </a><br>
  <a href="https://github.com/edunavajas/acta/releases/download/v0.1.0/acta-demo.mp4">▶ Watch the video (2 min, in Spanish)</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/oscuro/reunion-resumen.png">
    <img src="docs/screenshots/claro/reunion-resumen.png" alt="A meeting in acta: summary, decisions and tasks, each with its minute in the audio" width="860">
  </picture>
</p>

> The screenshots use demo data.

## What it is

Many meeting-minutes tools send your audio to some other company's cloud. acta does the opposite:

- **Records two separate tracks**: your microphone and the system audio (everyone else). It knows what you said without guessing, and only the others need to be told apart.
- **Survives unexpected shutdowns**: audio is written in segments while recording. If the app or the Mac dies, the meeting is recovered the next time you open it.
- **Transcribes and separates speakers on your Mac**, with models that run on the Neural Engine. The audio never leaves your disk.
- **Writes the minutes with the model you choose**: a local one (nothing leaves your machine) or a cloud one with your own key.

## What it does

| | |
|---|---|
| **Detects the call** | A floating pill tells you when a Teams or Meet meeting starts, and offers to stop when it ends. If you don't answer, it stops by itself after a while of silence. |
| **Transcript with speakers** | You, plus the others by name: it reads them from the Teams interface and remembers voices between meetings (always confirmed by you). |
| **Minutes** | Summary, decisions and action items. It separates **your tasks** from everyone else's. |
| **Outline and timeline** | The meeting as a tree of topics, with a jump to the exact minute of the audio. |
| **Screenshots** | Images of the shared screen, optional and off by default. |
| **Groups** | Group meetings by hand, or let acta suggest similar ones. |
| **Chat and search** | Ask about one meeting or a whole group; text and semantic search, all local. |

## Privacy: what leaves your Mac and what doesn't

| Data | Does it leave your Mac? |
|---|---|
| Audio | **No.** It stays in `~/Library/Application Support/acta`. |
| Transcription and speaker separation | **No.** Both run locally. |
| Transcript text (to write the minutes and for chat) | **It depends on the provider.** With a local model (built-in model, Ollama, LM Studio) it doesn't leave. With Anthropic, OpenAI, OpenRouter or another cloud service, the text is sent to that service. |
| Model downloads | Yes, the first time (speech models, search models and the built-in local model, if you use it). After that, transcription works offline. |

## Installation

### Homebrew (recommended)

```bash
brew tap edunavajas/acta https://github.com/edunavajas/acta
brew install --cask edunavajas/acta/acta
```

To update: `brew update && brew upgrade --cask acta`.

### Direct download

Download `acta-<version>-macos-arm64.zip` from the [latest release](https://github.com/edunavajas/acta/releases/latest), unzip it and drag `acta.app` to Applications.

### First launch (Gatekeeper)

acta is not signed or notarized, because the project has no Apple Developer account. That's why macOS blocks it the first time you open it if you downloaded it (Homebrew included). You only need to do this once:

- System Settings › Privacy & Security › **Open Anyway**, or
- in a terminal: `xattr -dr com.apple.quarantine /Applications/acta.app`

### Build from source

If you'd rather build it yourself, a single script does it in a few minutes.

**Requirements**

| What | Version |
|---|---|
| macOS | 14.2 or later, Apple Silicon |
| Xcode Command Line Tools | with Swift 6 (`swift --version`) |
| Rust | 1.91 or later (`rustup`) |
| Node and pnpm | pnpm 11 |

**Steps**

```bash
git clone https://github.com/edunavajas/acta.git
cd acta
scripts/install-mac.sh            # installs to ~/Applications/acta.app
# scripts/install-mac.sh --system # or to /Applications/acta.app
```

To **update**: `git pull` and run the script again. Since you build it yourself, macOS doesn't quarantine it and it should open straight away.

Script details, permissions and why it isn't signed: [docs/03-instalacion.md](docs/03-instalacion.md) (in Spanish). How a release is published: [docs/RELEASING.md](docs/RELEASING.md) (in Spanish).

## First launch

1. **Grant the permissions** when macOS asks: microphone and system audio. **Accessibility** (Teams names) is granted by hand in System Settings.
2. **Download the models** in Settings › Models (speech, speaker separation and voice detection).
3. **Choose who writes the minutes** in Settings › Providers:
   - the built-in local model, [Ollama](https://ollama.com) or [LM Studio](https://lmstudio.ai), to send nothing out;
   - [Anthropic](https://www.anthropic.com), [OpenAI](https://openai.com), [OpenRouter](https://openrouter.ai) or any OpenAI-API-compatible server, with your key;
   - the Claude Code, Codex or OpenCode CLI you already have installed. acta runs it as is, without touching its credentials: check that your provider's terms allow that use.

## How it's built

Hexagonal architecture in Rust: the domain does no I/O and everything that touches the system comes in through ports.

| Folder | What it is |
|---|---|
| `crates/acta-core` | Domain, ports and state machine (no I/O) |
| `crates/acta-audio` | Segmented WAV writing, recovery, auto-stop, supervisor and processing |
| `crates/acta-store` | SQLite + FTS5 + sqlite-vec, hybrid search, groups |
| `crates/acta-llm` | LLM providers, minutes generation and chat |
| `crates/acta-mac-bridge` | Rust client for the Swift sidecar |
| `sidecars/acta-mac` | Swift sidecar: audio capture, microphone, Accessibility, ScreenCaptureKit, FluidAudio |
| `apps/desktop` | Tauri v2 app (React + TypeScript): main window and floating pill |

To check that everything builds and the tests pass:

```sh
scripts/verify.sh
```

It runs the guards, `cargo check`, clippy, the Rust tests, `swift build`, `tsc` and `vitest`.

## Documentation

All of these are in Spanish.

- [docs/00-analisis.md](docs/00-analisis.md): analysis and baseline decision
- [docs/01-arquitectura.md](docs/01-arquitectura.md): architecture, contracts and implementation plan
- [docs/02-verificacion-manual.md](docs/02-verificacion-manual.md): manual tests
- [docs/03-instalacion.md](docs/03-instalacion.md): installation, permissions and configuration
- [docs/RELEASING.md](docs/RELEASING.md): how a release and the Homebrew cask are published
- [docs/referencia-meetily-zombis.md](docs/referencia-meetily-zombis.md): the zombie-process bug we must never repeat
- [docs/brand/GUIA-DE-USO.md](docs/brand/GUIA-DE-USO.md): logo usage guide

## Status

Version **0.1.0**, under development. macOS only for now; Windows is planned for later.

## License

acta is free software under the [GNU GPL v3.0 or later](LICENSE). You can use, study, modify and redistribute it. If you distribute a modified version, you must release it under the same license, with its source code.

The speech and language models acta downloads have their own licenses.

Third-party logos shown in the app belong to their owners: [NOTICE](apps/desktop/src/assets/logos/NOTICE.md).
