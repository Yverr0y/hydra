# hya-plugin

Hydra's embeddable plugin host, licensed MIT OR Apache-2.0: a Wasm runtime,
capability enforcement, package verification, plugin installation, and explicit
distribution indexes. It depends on the permissively licensed transport and
contract crates, with no dependency on the GPL application crates.

```toml
[dependencies]
hya-plugin = "0.1"
```

Use `manager::Manager` for the installed plugin lifecycle, `host::Frontend`
to supply forms and activity reporting, and `runtime::Runtime` for guest ABI
validation. Plugin metadata is untrusted input; authors and signing keys are
separate fields. Packages declare capabilities that the host must grant before
use.

The published package carries the Hydra product version from the source
checkout to evaluate `min_hydra`. Before packaging from this repository, run
`python3 scripts/plugins/stage-crates.py`; the publish workflow does this
automatically. This bundles the version file without requiring a workspace
manifest on a downstream machine.

See the [plugin guide](https://github.com/ja7ad/hydra/tree/main/plugins).
