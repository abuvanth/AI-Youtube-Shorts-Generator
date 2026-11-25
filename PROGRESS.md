# Progress

## Completed

- Audited the existing Rust pipeline and Rune studio. Preserved the CLI, local and MuAPI modes, credential storage, transcription, and highlight ranking.
- Highlight candidates that do not overlap the real transcript are discarded. Categories are parsed from the model response. Near-equal scores prefer clips spread across the timeline.
- Local export can burn transcript captions (ASS plus SRT) in six presets and can show a text hook only when its words appear in the clip. Karaoke styling is phrase-level. Word timings are not invented.
- Vertical framing supports center crop and blurred-background fill. Face tracking is not implemented. FFmpeg errors include the process output, and each local export is checked with ffprobe.
- Finished generations can be saved under the output folder and reopened. Project files do not contain API keys.
- MOV is accepted alongside MP4, MKV, and WebM.
- The studio exposes length presets, captions, framing, and opening mode. Cloud mode does not pretend to apply those local controls.
- A selected local clip can be trimmed and exported again from the source file. That render uses the current caption, framing, and hook settings and does not call Gemini. Invalid ranges and missing source files do not start a worker.

## In progress

- None in this pass.

## Remaining

- Smart crop, speaker tracking, split screen, and manual crop keyframes.
- ElevenLabs spoken hooks. Scribe transcription remains available. Text-to-speech is not wired.
- The rest of the clip editor: split, reorder, waveform timeline, undo, and direct subtitle-line editing.
- Durable SQLite batch queue with pause, retry, and restart.
- License activation. No licensing code is included, so a development bypass cannot ship by accident.
- Dashboard, projects browser, batch queue, and export-history screens beyond the current studio and saved-project list.
- Word-level caption alignment.

## Tests run and results

- `cargo fmt --all`
- `cargo +stable-x86_64-pc-windows-msvc test --offline --lib` on 25 Nov 2026, after trim re-export: **132 passed, 0 failed, 1 ignored**. The ignored test renders GPU snapshots without a desktop window.
- `cargo +stable-x86_64-pc-windows-msvc test --offline --bins` on 25 Nov 2026: **1 passed, 0 failed**. The GUI binary target has no unit tests.
- The ignored snapshot test was not run.
- `cargo +stable-x86_64-pc-windows-msvc clippy --offline --all-targets -- -W clippy::all` on 25 Nov 2026: no warnings in this crate after the trim-export cleanup. Vendor crates `wgpu-hal` and `parley_engine` still warn.
- An earlier lib run failed one GUI test because a longer mode description pushed the quality control out of view. The description was restored. The rerun above includes that test and passes.
- The synthetic FFmpeg test `renders_a_blurred_vertical_short_when_ffmpeg_is_installed` passed. It builds a 640×360 source, exports 360×640 with blur fill and Tamil/Hindi captions, and checks duration, size, and audio with ffprobe.
- Default GNU toolchain could not link (`dlltool.exe` missing). The MSVC toolchain with Visual Studio 2022 Build Tools was used instead.

## Known issues

- Default `rustup` toolchain is `stable-x86_64-pc-windows-gnu` and cannot link this crate until `dlltool` is installed. Use `+stable-x86_64-pc-windows-msvc` with the Build Tools environment.
- CLI caption burning stays off so existing commands keep the previous picture. The desktop app defaults captions on.
- MuAPI mode does not burn local captions or apply local reframing.
- Input seeking is keyframe-based. Export duration is allowed to differ by up to 8% or 1.5 seconds.
- Saved projects remember the result and paths. They do not recreate missing video files.

## Decisions and tradeoffs

- Extended the existing studio instead of replacing it with unfinished navigation screens.
- Center crop remains the default framing so current output does not change unless the user selects blur fill.
- Text hooks are refused when they are not grounded in the transcript.
- No license or payment backend was added.
