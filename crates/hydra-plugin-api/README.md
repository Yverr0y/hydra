# hya-plugin-api

The shared, MIT OR Apache-2.0 contract between Hydra plugin hosts and Wasm
resolver plugins. This crate contains manifests, download plans, typed forms,
permissions, errors, selection rules, and the versioned ABI constants. It has
no transport or runtime dependency.

```toml
[dependencies]
hya-plugin-api = "0.1"
```

Plugin authors normally use `hya-plugin-sdk`, which re-exports these types.
Host implementers use the contract directly. An optional `author` manifest
field supplies the display name; it does not authenticate a publisher.

See the [plugin guide](https://github.com/ja7ad/hydra/tree/main/plugins)
and [SDK guide](https://github.com/ja7ad/hydra/blob/main/plugins/sdk/README.md).
