# Standalone anonymous YouTube E2E

This test runs yt-dlp outside Hydra, without importing browser cookies. It
enumerates the reported playlist and attempts actual media downloads for its
first three entries and the reported failing video. Success requires a nonempty
download with an audio or video stream verified by ffprobe, and confirmation
that the PO-token provider loaded. Listing a playlist alone is not success.

## Observed result: 2026-10-05

The playlist returned 31 entries. Stable yt-dlp 2026.08.19 and nightly
2026.09.27.232945 rejected the video `9KkbfDVQYO0` with bot verification.
The nightly also rejected the first three playlist entries. bgutil 2.0.1
generated player PO tokens for the mweb requests, but the player response
remained `LOGIN_REQUIRED`. The web and android_vr clients also failed.
An additional download attempt with Chrome request impersonation failed.

The alternative WebPoClient provider 1.1.2 successfully minted a guest player
PO token in a separate browser session, but the same video still failed.
No actual anonymous media download succeeded. This setup is a reproducible
diagnostic, not a verified workaround or a permanent fix.

No configured proxy endpoint was available, and IPv6 could not connect to
YouTube. A comparison using another connection remains untested; the results
do not prove that the IP address is the cause.

## Reproduce

Requires uv, Git, Node.js, npm, Deno, and ffprobe on PATH. Run from the repository
root. The installation below stays in a temporary directory.

```sh
work_dir=$(mktemp -d)
uv venv "$work_dir/venv"
uv pip install --python "$work_dir/venv/bin/python" \
  'yt-dlp[default]==2026.9.27.232945.dev0' \
  'bgutil-ytdlp-pot-provider==2.0.1'
git clone --depth 1 --branch 2.0.1 \
  https://github.com/Brainicism/bgutil-ytdlp-pot-provider.git \
  "$work_dir/provider"
(cd "$work_dir/provider/server" && npm ci && npx tsc)
node "$work_dir/provider/server/build/main.js" --host 127.0.0.1 --port 4417
```

Keep the provider running. In another Terminal, use the same `work_dir` path:

```sh
python3 scripts/youtube-anonymous-e2e.py \
  --python "$work_dir/venv/bin/python" \
  --plugin-dir "$work_dir/provider"
```

To test a supplied proxy, append `--proxy 'http://127.0.0.1:7890'` with the
actual endpoint. The provider receives the route from yt-dlp. An empty proxy
argument uses the current connection without an explicit application proxy;
it does not disable a system VPN.

Each run preserves stdout, stderr, downloaded files, ffprobe results and a
JSON report in its printed artifact directory. A failed download, timeout,
invalid media file, or missing provider produces a nonzero exit status.
The script never imports account cookies or changes the Homebrew installation.

The report-validation unit tests use controlled subprocess replies. These
check false-success detection and are separate from the live network E2E:

```sh
python3 scripts/test_youtube_anonymous_e2e.py
```

References: [yt-dlp EJS setup](https://github.com/yt-dlp/yt-dlp/wiki/EJS),
[PO-token guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide),
[bgutil provider](https://github.com/Brainicism/bgutil-ytdlp-pot-provider),
[WebPoClient provider](https://github.com/coletdjnz/yt-dlp-getpot-wpc).
