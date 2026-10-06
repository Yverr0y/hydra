# Plugin catalog end-to-end checks

Debug builds of the CLI and GUI accept
`--debug-plugin-catalog http://127.0.0.1:PORT/plugins.json` (the GUI also accepts
`--debug-plugin-catalog=URL`). This replaces the default catalog only for that
process. The override is never saved in the user's index list.

The test catalog can use `localhost` or an IPv4 loopback address. HTTP downloads
are allowed only from that explicitly selected origin,
including its port. Ordinary builds without the option still require HTTPS;
release builds do not expose the CLI option and reject the GUI option. Plugin
guest HTTP permissions, package verification and permission consent are unchanged.

## Automated CLI scenario

```sh
python3 scripts/plugins/catalog-e2e.py
```

The harness builds the real CLI and creates a temporary profile, HTTP server,
two valid `.hyaplugin` packages containing a real Wasm module, and a schema-1
JSON catalog. It verifies:

- The temporary default is present and cannot be removed or replaced.
- HTTP installation is refused without the debug option.
- Installation requires `--accept-permissions` and downloads version `1.0.0`.
- A mirror advertising `9.0.0` cannot override the default's `1.1.0` update.
- Listing updates does not install them.
- A mismatched catalog checksum refuses the update and keeps the old version.
- Applying the catalog update downloads `1.1.0`, preserves settings and records
  a rollback version; rollback restores `1.0.0`.

The same scenario runs in the CLI integration suite using the test build of the
binary. To run only it:

```sh
cargo test -p hya-cli --test plugins real_cli_installs_and_updates
```

## Real GUI scenario

```sh
python3 scripts/plugins/catalog-e2e.py --gui
```

After the CLI scenario, the harness resets the served catalog to `1.0.0` and
launches the real GUI with a separate temporary profile. It prints the catalog,
installation, update-control URLs and the profile path. Keep it running while
testing the GUI:

1. Open Options → Extensions & Plugins → Plugins. Confirm that the local
   default URL is marked **Default catalog (read-only)** and has no remove button.
2. Paste the printed `install` URL into the package source field. Choose
   **Review permissions**, then **Accept permissions and install**. Confirm
   **Catalog E2E Plugin** appears with version `1.0.0` in plugin information.
3. Publish the next catalog version using the printed control URL:

   ```sh
   curl -X POST http://127.0.0.1:PORT/publish/1.1.0
   ```

4. Choose **Check plugin updates**, or the plugin row's **Check for updates**
   icon. Confirm that its row shows **Plugin update available · 1.1.0** and
   the icon changes to **Install plugin update**. Click that icon, review the
   new permissions and approve installation. Confirm plugin information shows
   `1.1.0`, the badge disappears and the icon returns to **Check for updates**.
5. Check the persisted result from another terminal, using the printed profile:

   ```sh
   HYDRA_CONFIG_DIR=/tmp/hydra-catalog-e2e-EXAMPLE/gui-profile \
     target/debug/hydra plugin info example.catalog-e2e --json
   ```

The installed manifest should be `1.1.0`, and its `previous` manifest should be
`1.0.0`. Close the test GUI or stop the harness to remove the temporary profile,
packages and server. The regular application profile is never used.
