# YouTube resolver

This official Hydra plugin uses [yt-dlp](https://github.com/yt-dlp/yt-dlp)
to extract metadata. Hydra downloads direct HTTP media tracks and combines
video and audio through your installed ffmpeg. Playlist links expand into ordered
video jobs (up to 512 available entries). Audio can be saved in its original format
or converted to MP3, M4A, Opus, FLAC or WAV using ffmpeg. Live streams, DRM formats
and formats available only as segmented manifests are not supported.

Install yt-dlp for your platform and put `yt-dlp` (`yt-dlp.exe` on Windows)
on PATH, or enable **Download missing yt-dlp backend** in the plugin settings.
That opt-in downloads the official 2026.08.19 release for the host platform,
verifies its published SHA256 and pins it in the plugin data directory.
Existing pinned programs are never replaced automatically. No helpers are bundled. The Wasm plugin is identical on Linux, macOS and
Windows, including ARM64; the external tools must match your OS and CPU.

```sh
rustup target add wasm32-wasip1
cargo build --manifest-path plugins/hydra-youtube/Cargo.toml --release --target wasm32-wasip1
```

Copy `target/wasm32-wasip1/release/hydra_youtube.wasm` from this folder to
`plugin.wasm`, then install this folder in developer mode, or use the packaging
script in the parent README.

```sh
hydra plugin install plugins/hydra-youtube --accept-permissions
hydra --list-tracks 'https://www.youtube.com/watch?v=VIDEO_ID'
hydra --quality 1080 --container mkv 'https://www.youtube.com/watch?v=VIDEO_ID'
hydra --no-input --extract-audio --audio-format mp3 'https://www.youtube.com/playlist?list=PLAYLIST_ID'
hydra --audio none --track FORMAT_ID 'https://www.youtube.com/watch?v=VIDEO_ID'
```

GUI: Options → Extensions & Plugins → Plugins → enter this folder/package → Review permissions →
Accept permissions and install. Add URL automatically inspects matching links. Choose
video, audio, subtitle tracks and the output container before starting.
Playlist entries can be selected before they enter the queue. The Audio only option
shows extraction formats. Subtitles are saved as sidecar files. Choose Node or Deno in plugin settings
when yt-dlp needs an installed JavaScript runtime.

Cookies are optional and disabled by default. To enable them, set `use_cookies`
to true and supply browser cookies using Hydra's existing cookie controls.
Only granted cookie domains enter a temporary Netscape file, removed after the
tool exits. Download only content you are authorized to save.

The helper runs with `--ignore-config` and `--no-plugin-dirs`, and receives
`--skip-download`; local yt-dlp configuration cannot add execution or download
options. yt-dlp is pinned by file hash. After updating it, explicitly approve
its new binary with `hydra plugin grant hydra.youtube exec:yt-dlp`.
Use `hydra plugin set hydra.youtube yt_dlp_path /path/to/program/folder`
followed by that grant command when it is outside PATH.

A terminal download without explicit selection flags offers numbered quality and audio
choices. Use `--no-input` for unattended downloads; it also refuses plugin forms.
Use `hydra plugin logs hydra.youtube` or View plugin logs in GUI settings for
resolver diagnostics. Installation shows optional setup hints, which remain in plugin info.

Development tests:

```sh
cargo test --manifest-path plugins/hydra-youtube/Cargo.toml
python3 scripts/plugin-media-e2e.py
```

The final package is `youtube-download.hyaplugin`. Automatic backend installation
supports macOS x64/ARM64, Windows x64/ARM64 and Linux x64/ARM64. Other platforms
can provide their own yt-dlp executable. Hydra's configured proxy is supplied to
metadata requests and the backend through its otherwise scrubbed environment.
The fixed backend release is reproducible; updating it is an explicit plugin
maintenance change, since extractor compatibility can change at YouTube.
