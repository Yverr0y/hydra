# hya-plugin-sdk

Rust guest SDK for Hydra Wasm resolver plugins, licensed MIT OR Apache-2.0.
It re-exports `hya-plugin-api`, provides typed host calls, and exports the guest
ABI with `export!`. Each hook runs in a fresh Wasm instance.

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
hya-plugin-sdk = "0.1"
```

```rust
use hya_plugin_sdk::{Host, Plan, Plugin, Resolve, ResolveRequest, Result, Track};

#[derive(Default)]
struct Resolver;

impl Plugin for Resolver {
    fn resolve(&mut self, _: &Host, request: ResolveRequest) -> Result<Resolve> {
        Ok(Resolve::Plan(Plan::single("Download", Track::file("file", request.url))))
    }
}

hya_plugin_sdk::export!(Resolver);
```

Build with `cargo build --release --target wasm32-wasip1`, then supply
`hydra-plugin.toml` and package with `hydra-plugin pack`.
See the [SDK guide](https://github.com/ja7ad/hydra/blob/main/plugins/sdk/README.md)
for permissions, hooks, and other supported languages.
