# Shared plugin catalog

[`../plugins.json`](../plugins.json) is the single catalog used by the website,
GUI and CLI. Its public URL is `https://hydra.javad.dev/plugins.json`.
The app always includes this default catalog; users can add and remove other
HTTPS catalogs, but cannot remove or replace the default.

The file has `schema: 1`, a catalog `name`, and a `plugins` array. Each entry has:

| Field | Meaning |
| --- | --- |
| `id` | Unique ID from the package manifest, e.g. `community.video` |
| `name` | Display name |
| `description` | What the plugin does and any external tools it needs |
| `version` | Plugin SemVer from the manifest, not Hydra's release tag |
| `is_official` | `true` for Hydra-maintained plugins; community submissions use `false` |
| `download` | Direct HTTPS URL of the released `.hyaplugin` package |
| `homepage` | Source repository or project homepage |
| `author` | Creator or team name |
| `image` | Artwork path relative to `docs/`, inside `plugins/img/{id}/` |
| `api` | Plugin API major version from the manifest (currently `1`) |
| `sha256` | Optional SHA-256 of the complete released archive |
| `publisher_key` | Optional Minisign public signing key, matching the package manifest |
| `min_hydra` | Optional minimum Hydra version |

`sha256` detects a changed or damaged download. It is not a secret and is not
a signing key. `publisher_key` identifies the signer; the matching private key
stays with the creator. Omitting either field is supported. When provided,
Hydra enforces it before installation. The package's own checksums and any
signature are still verified, and permissions still require consent.

## Submit a community plugin

1. Build and validate your plugin with `hydra-plugin`. Use your own stable ID
   and increase its SemVer when you release an update.
2. Publish the `.hyaplugin` package at a direct HTTPS URL, for example a GitHub
   release asset. Keep old versioned assets available.
3. Add a record to `docs/plugins.json` with `is_official: false` and add your
   artwork in `docs/plugins/img/{id}/`.
4. Open a pull request with a link to the source, installation instructions,
   and a description of the permissions and external programs it needs.
   Maintainers review submissions before including them.

Example record without the optional pins:

```json
{
  "id": "community.video",
  "name": "Community Video Downloader",
  "description": "Download videos from Example Video.",
  "version": "1.0.0",
  "is_official": false,
  "download": "https://example.com/releases/community-video-1.0.0.hyaplugin",
  "homepage": "https://example.com/community-video",
  "author": "Plugin Creator",
  "image": "plugins/img/community.video/icon.svg",
  "api": 1
}
```

### Add a checksum and signing key

Generate your own Minisign key pair and sign the package. This example uses an
encrypted private key. Set `HYDRA_PLUGIN_KEY_PASSWORD` using a password prompt
before signing; its value is the password chosen during key generation.

```sh
minisign -G -p publisher.pub -s publisher.key
hydra-plugin build my-plugin --output plugin-unsigned.hyaplugin
hydra-plugin sign plugin-unsigned.hyaplugin \
  --secret-key publisher.key --public-key publisher.pub \
  --password-env HYDRA_PLUGIN_KEY_PASSWORD --output plugin.hyaplugin
hydra-plugin validate plugin.hyaplugin
shasum -a 256 plugin.hyaplugin
```

Copy the checksum into `sha256`. Copy the base64 public-key line from
`publisher.pub` into `publisher_key`; omit its `untrusted comment:` line.
Hydra's signing command also embeds that same key in the package manifest.
Compute the checksum **after signing**, since signing changes the archive.
Keep `publisher.key` private and reuse the same key for subsequent releases.
On Windows, `Get-FileHash plugin.hyaplugin -Algorithm SHA256` gives the checksum.

## Mirrors and updates

Users can add JSON catalogs or existing schema-1 TOML indexes in the GUI's
Plugin indexes section, or with `hydra plugin index add HTTPS_URL`. Custom
indexes can optionally have a detached `.minisig` signature next to the index;
the trusted index key is separate from a plugin's publisher key.

The default catalog takes precedence for duplicate IDs, even if a mirror
advertises a higher version. Mirrors provide plugins absent from the default;
when multiple mirrors provide an ID, Hydra selects the newest compatible
version. Updates never downgrade installed plugins.

In the GUI, choose **Check plugin updates**, or check one plugin using its row
icon. Available versions appear as badges beside each plugin. Click the
**Install plugin update** icon and approve the package permissions. In the CLI:

```sh
hydra plugin update
hydra plugin update community.video --accept-permissions
```

The first command lists available updates. The second applies the selected
update after explicit consent. A publisher change also requires
`--accept-publisher-change`; check the reported publisher before accepting it.

Install links use `hydra://install-plugin?url={encoded-package-url}` and
require a desktop build with the protocol registered. They fetch and inspect
the package, then open permission review. Browsers cannot reliably detect a
protocol handler; the page also offers direct download and manual installation.

Preview with an HTTP server rooted at `docs/`, then open `plugins.html`.
Run catalog checks with `node --test scripts/tests/plugin-catalog.test.mjs`.

For an isolated localhost catalog and real CLI/GUI installation and update
checks, see [the E2E guide](../testing/plugin-catalog-e2e.md). Local HTTP support
is available only through the explicit `--debug-plugin-catalog` option in debug
builds, and is limited to the selected loopback origin.
