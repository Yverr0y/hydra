# Plugin authoring and CLI E2E

Run from the repository root:

```sh
rustup target add wasm32-wasip1
cargo test -p hya-plugin-cli -- --include-ignored
cargo test -p hya-cli --test plugins
cargo build -p hya-cli -p hya-plugin-cli
python3 scripts/plugins/cli-e2e.py
```

The Python E2E requires a Unix pseudo-terminal (Linux or macOS). It launches
real executables in an isolated profile. It answers the interactive init wizard,
checks ID, version and author metadata, builds a fresh Rust Wasm plugin, packs
and validates it, tests permission consent, and installs it. A local HTTP server
provides the resolved payload, whose downloaded bytes are compared exactly.
It then opens Hydra's interactive TUI, checks plugin name, ID, author and version,
toggles the plugin off and on, checks persisted state, quits, and removes it.

The Rust authoring tests exercise all five language scaffolds, custom metadata,
existing-directory refusal, non-terminal wizard refusal, malformed packages,
invalid guest ABI, and a real Rust compiler build. Unit tests exercise wizard
retries, suggestions and cancellation. The CLI integration tests additionally
exercise settings, subtitles, grants, source refresh after HTTP 403, cleanup,
JSON output, missing authors and terminal-control sanitization.

## Observed on 2026-10-05

These executable scenarios passed locally on macOS ARM64. The release workflow
runs authoring and CLI integration tests on Linux, macOS and Windows, plus the
pseudo-terminal E2E on Linux and macOS. Builds produce separate authoring-tool
archives for x64 and ARM64 on all three platforms; Linux CLI archives use musl.
The hosted cross-platform builds must still run in GitHub Actions.

`make fmt`, `make lint`, and `make test` passed. Scoped coverage tests covered
all branches in the display-name validation and safe project-name derivation.
The terminal E2E accepted `Youtube test`, preserved it in the manifest, and a
fresh Rust scaffold compiled under its safe project name `youtube-test`.

The manual signing recipe in `crates/hydra-plugin-cli/README.md` was exercised
with Minisign 0.12 in a temporary directory. The signed archive validated as
`Verified`; changing its manifest and regenerating checksums without signing
again failed signature verification. The unpacked development folder reported
`Unsigned`, as expected. The temporary test signing key was removed.


Package checks compiled all four plugin crate archives in an isolated temporary
workspace, using local patches for the unpublished dependencies. The extracted
CLI archive passed its tests and built a new Rust scaffold without the repository
SDK. These checks validate bundled sources; they do not publish to crates.io.

Before packaging crates, `python3 scripts/plugins/stage-crates.py` copies SDK
sources into the CLI package and records the product version for the host.
Nested SDK manifests are bundled as `Cargo.toml.template`, then restored to
`Cargo.toml` during scaffolding, because Cargo excludes nested packages from
crate archives. The publish workflow stages automatically and publishes in
order: API, SDK, host, authoring CLI, after the transport dependencies.

These tests use a controlled plugin and local server. Live YouTube backend
results are tracked separately in `youtube-anonymous-e2e.md`.

## Official release signing

The release signing path was tested locally with the official YouTube package
and real Minisign 0.12. `scripts/plugins/signing-e2e.py` generates disposable keys
and verifies successful signing, missing secrets, malformed keys/packages,
mismatched public/private keys, re-signing refusal, source preservation, and
manifest tampering with recomputed checksums. These cases run through the actual `hydra-plugin sign` executable, and
Minisign independently verifies its resulting signature. Rust executable tests
also cover encrypted private keys and correct/incorrect password environments.
The test installs signed and unsigned packages in an isolated profile, checks
`hydra plugin info` and `--json`, and verifies green signed text through a PTY.

The newly generated official key also signed the package successfully;
`hydra-plugin validate` reported `Verified` with fingerprint
`b567c8a9f94ea0a5`. Installing that package with `hydra plugin install` in a
throwaway `HYDRA_CONFIG_DIR` succeeded and retained the official publisher key.
The matching private key was stored outside the repository and configured as
GitHub repository Actions secret `HYDRA_PLUGIN_SIGNING_KEY`. No release workflow
was dispatched during this test.

The final `make fmt`, `make lint`, and `make test` gate passed. The workspace
coverage run also passed; a clean focused report measured 97.14% line coverage
and 91.78% region coverage for the new native signing module. The CLI info
formatter had all 34 regions exercised. Light and dark GUI renders were
inspected with the verified badge, fingerprint, and explicit unsigned state.
The rebuilt release authoring binary passed the same end-to-end signing test.

The older CLI lifecycle and PTY TUI scripts also passed after updating their
repository-root lookup for `scripts/plugins/` and using explicit `--json` when
parsing list/info metadata. These runs covered consent, settings, secret masking,
download hashes, enable/disable, grants, and removal.
