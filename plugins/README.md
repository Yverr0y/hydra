# Official Hydra plugins

Official plugin folders are standalone Rust workspaces targeting `wasm32-wasip1`.
Language SDKs are in [sdk](sdk/README.md); `hydra-plugin` scaffolds Rust, Python,
JavaScript, C and Go projects.
Plugins are installed explicitly and are not bundled with Hydra installers.
Hydra's core crates contain no service-specific extractors or host lists.

Build and package a plugin (Linux, macOS, Windows):

```sh
rustup target add wasm32-wasip1
python3 plugins/build.py hydra-youtube
hydra plugin inspect plugins/hydra-youtube/youtube-download.hyaplugin
hydra plugin install plugins/hydra-youtube/youtube-download.hyaplugin --accept-permissions
```

On Windows, use `python` if that is the installed Python command. Alternatively,
run the Cargo build command from the plugin README, copy the Wasm file beside
its manifest as `plugin.wasm`, then use `hydra plugin pack FOLDER OUTPUT.hyaplugin`.
The GUI accepts the same packages and development folders in Options → Extensions & Plugins → Plugins.

GUI and CLI share installed plugins under `~/.config/hydra/plugins/{plugin_id}`
on Unix and `%APPDATA%\hydra\plugins\{plugin_id}` on Windows. `HYDRA_CONFIG_DIR`
selects an isolated profile for tests. Both fronts use the same typed settings;
the GUI presents controls and the CLI presents terminal choices.

A manifest may include `author = "Your name"` for CLI, TUI and GUI display.
This display name is separate from the publisher signing key.

A manifest may include optional top-level `welcome = "Setup instructions…"`.
Hydra shows this text after a successful installation and retains it in plugin info.
Use it for required tools or first-run steps. It is plain text, bounded like other
manifest strings, and never executes setup commands.

Installed packages can be opened from the file manager when the packaged Hydra GUI
is registered as the `.hyaplugin` handler. File opening starts permission review.
It also works with `hydra-gui --install-plugin FILE.hyaplugin` or a bare package path.

## Official release signing

The release workflow builds `hydra-youtube`, signs its `.hyaplugin` package, and
publishes `youtube-download.hyaplugin` and `hydra-official-plugins.pub` as release
assets. `official.pub` is the official publisher's public key. The matching
private key lives in the repository Actions secret `HYDRA_PLUGIN_SIGNING_KEY`;
it is exposed only to the signing step. Missing or mismatched keys fail the job.
CI tests signing with disposable keys and never needs the official secret.

To create a new publisher key (requires [Minisign](https://jedisct1.github.io/minisign/)):

```sh
mkdir -p ~/.config/hydra/signing
chmod 700 ~/.config/hydra/signing
minisign -G -W -p plugins/official.pub -s ~/.config/hydra/signing/official.key
chmod 600 ~/.config/hydra/signing/official.key
gh secret set HYDRA_PLUGIN_SIGNING_KEY --repo ja7ad/hydra < ~/.config/hydra/signing/official.key
```

This creates an unencrypted private key for unattended signing. Keep a secure
backup outside the repository. Do not regenerate it for each release: changing
the public key changes the publisher fingerprint shown to users on updates.
Never add private keys to Git. Key generation refuses to overwrite existing keys.

Build and sign locally with the same command used by Actions:

```sh
python3 plugins/build.py hydra-youtube
HYDRA_PLUGIN_SIGNING_KEY="$(cat ~/.config/hydra/signing/official.key)" \
  target/debug/hydra-plugin sign plugins/hydra-youtube/youtube-download.hyaplugin \
    --output target/signed-plugins/youtube-download.hyaplugin \
    --public-key plugins/official.pub --key-env HYDRA_PLUGIN_SIGNING_KEY
target/debug/hydra-plugin validate target/signed-plugins/youtube-download.hyaplugin
python3 scripts/plugins/signing-e2e.py
```

The command validates the input, adds the publisher key to a staged manifest,
recomputes all checksums, signs `SHA256SUMS`, checks the signature against
`official.pub`, and validates the final archive before writing the output.
It leaves the original manifest and unsigned package unchanged. A verified
signature proves possession of the declared publisher key; users can compare
its fingerprint with the official public key. It does not grant permissions.

`hydra plugin info PLUGIN_ID` shows `Signed (verified)` and the publisher's
SHA-256 fingerprint, or `Unsigned`. Signed text is green in a terminal;
`NO_COLOR` disables color. Use `hydra plugin info PLUGIN_ID --json` for complete
metadata in scripts. The GUI's plugin information and installation review show
a verified badge and the same fingerprint. Development folders remain unsigned,
even if their manifest declares a publisher key.
