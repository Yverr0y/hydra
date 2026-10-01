# hydra-plugin (hya-plugin-cli)

Standalone authoring CLI for Hydra Wasm plugins, licensed MIT OR Apache-2.0.
Available as `hydra-plugin` in Linux, macOS, and Windows release archives, or
with `cargo install hya-plugin-cli --locked` after publication.

Run `hydra-plugin init` in a terminal for a wizard that asks for the name,
suggested plugin ID, directory, version, author name, and language. Press Enter
to accept a suggestion. Invalid answers can be retried; closing input cancels
before creating files. Initialization prints the created directory, SDK and
source-file progress, followed by the build command.

```sh
hydra-plugin init youtube-test --name "Youtube test" --language rust --author "Your name"
rustup target add wasm32-wasip1
hydra-plugin build youtube-test
hydra-plugin validate youtube-test/youtube-test.hyaplugin
hydra-plugin pack youtube-test --output youtube-test.hyaplugin
```

`init <path>` runs without prompts; `--interactive` enables the wizard with
pre-filled flags. Display names may contain spaces and uppercase letters: `Youtube test`
suggests `example.youtube-test` and a `youtube-test` directory. The generated
Cargo project and default archive use the safe name `youtube-test`.
`--id`, `--plugin-version`, and `--author` override metadata.
`init` supports Rust, Python, Node.js, C, and Go and vendors the SDK into the
project. It refuses to overwrite an existing directory. `build` compiles,
validates, and packages the project. `pack` validates and packages an already
compiled directory without invoking a compiler. `validate` checks metadata,
package integrity and guest ABI without granting plugin capabilities.

Set `author = "Your name"` in `hydra-plugin.toml` to identify the author in
Hydra's CLI, TUI and GUI. Install packages with
`hydra plugin install my-resolver.hyaplugin --accept-permissions` after reviewing
`hydra plugin inspect`. `hydra plugin list` prints a short table;
`hydra plugin list --json` prints complete installed metadata.

Build prerequisites vary by language. See the
[SDK guide](https://github.com/ja7ad/hydra/blob/main/plugins/sdk/README.md).
Before packaging this crate from the repository, run
`python3 scripts/plugins/stage-crates.py` to bundle the SDK sources. Published
crates and release binaries include these assets and need no Hydra checkout.

End-to-end authoring tests (requires the Rust Wasm target):

```sh
cargo test -p hya-plugin-cli --test authoring -- --include-ignored
```

## Packing and signatures

`build` compiles `plugin.wasm` and also packs it. `pack` skips compilation: it
reads the existing manifest and the module named by that manifest, validates
them, generates `SHA256SUMS`, and writes a ZIP-format `.hyaplugin` archive.
It does not archive the project source or the vendored SDK.

```sh
hydra-plugin pack my-plugin-1 --output my-plugin-1.hyaplugin
hydra-plugin validate my-plugin-1.hyaplugin
```

The output is a filename, not a directory. `--output ./test` writes an archive
named `test`; the tool does not automatically add `.hyaplugin`. Prefer the
extension for file association. A module must already exist; rebuild after
editing the source before packing.

`Unsigned` means the package has no verified publisher signature. Checksums
check integrity, but author names and checksums do not authenticate a publisher.
`pack` produces unsigned packages. `sign` adds a publisher key and a
[Minisign](https://jedisct1.github.io/minisign/) signature over `SHA256SUMS`, which
covers both the manifest and Wasm module. Signing runs natively and needs no
Minisign executable, Python, or Rust compiler.

Create a publisher key pair once with Minisign:

```sh
minisign -G -p publisher.pub -s publisher.key
```

Keep `publisher.key` private and reuse it for releases from the same publisher.
For an encrypted key, set a password environment variable and name it with
`--password-env`. The password is never passed as a command-line argument.

```sh
hydra-plugin sign my-plugin-1.hyaplugin --secret-key publisher.key \
  --public-key publisher.pub --password-env PLUGIN_KEY_PASSWORD \
  --output my-plugin-1-signed.hyaplugin
hydra-plugin validate my-plugin-1-signed.hyaplugin
```

Omit `--password-env` for an unencrypted key generated with `minisign -G -W`.
For unattended signing, use `--key-env HYDRA_PLUGIN_SIGNING_KEY` instead of
`--secret-key`; that variable must contain the complete private-key file.
The two key source options are mutually exclusive. A missing key or password,
a mismatched public key, an invalid package, or an already signed input is
rejected. Build a fresh unsigned package when releasing changes. Output must
differ from input; it is written only after verification, preserving the source.

The archive's files must be at its root. Successful signature validation reports
`Verified` with a publisher-key fingerprint. `validate signed-plugin` treats the
folder as a development plugin and reports `Unsigned`; verify the archive to
check its signature. Editing any signed file requires new checksums and a new
signature. Do not run `pack` on the signed staging folder: it refuses
`publisher_key` and would otherwise rebuild an unsigned archive.
