# Hydra plugin SDKs

All languages produce **one core Wasm module** with Hydra API 1. Packages use
`.hyaplugin`. The Rust SDK lives in `crates/hydra-plugin-sdk`; additional language
SDKs live here. A plugin never runs inside the host's Python or Node process.

| Language | Guest implementation | Build prerequisites |
| --- | --- | --- |
| Rust | Rust SDK | Rust, `rustup target add wasm32-wasip1` |
| C | `c/hydra.h` and `c/hydra.c` | [wasi-sdk](https://github.com/WebAssembly/wasi-sdk), `WASI_SDK_PATH` |
| Go | `go/hydra.go` | Go 1.24+ (`GOOS=wasip1 GOARCH=wasm`, `-buildmode=c-shared`) |
| JavaScript | Boa embedded in Wasm | Node.js 18+, Rust and `wasm32-wasip1` |
| Python | RustPython embedded in Wasm | Python 3.8+, Rust 1.95+ and `wasm32-wasip1` |

## Authoring tool

Install from this checkout:

```sh
cargo install --path crates/hydra-plugin-cli
hydra-plugin init my-plugin --language python
hydra-plugin build my-plugin
hydra-plugin validate my-plugin/my-plugin.hyaplugin
```

Languages: `rust`, `python`, `nodejs` (alias `javascript`), `c`, `go`.
Each scaffold vendors the SDK sources, so building it does not require the Hydra
checkout. Keep `.hydra-sdk` in version control. Edit `hydra-plugin.toml` to set
identity, claims, permissions, hooks and settings. `hydra-project.toml` records
build language and output name. Build invokes the language compiler with argument
arrays and validates the module before packing it. Validate checks checksums,
signatures when present, manifest rules, claims, permissions, imports, exports,
memory bounds and API version. It does not run a resolver or grant capabilities.
Builds need network access for compiler dependencies on first use. Modules and
packages can then run on Linux, macOS and Windows without those build tools.

## Python and JavaScript

Define synchronous `resolve(request)` and optionally `check()` for prerequisite checks. Return `hydra.file(request.url)` in JS or
`hydra.file(request["url"])` in Python, a `{"plan": ...}` object for multiple
tracks, or `"not_claimed"`. `hydra` is injected, with `settings()`, `exec(program,
args)`, `prompt(form)`, `log(level, message)` and generic `call(name, request)`.
Host errors become language exceptions. The default refresh resolves again and
checks plan and track identities.

These are sandboxed language runtimes: Node is **build tooling**, not the guest
runtime. Node modules (`fs`, `net`, `child_process`), npm native addons, CPython
extensions and pip packages are not available. Python has builtins and the
interpreter's built-in modules, not the full CPython standard library. Bundle
pure source into the entry file. JavaScript has ECMAScript globals, not DOM or
Node globals. Use Hydra host calls for I/O. Both interpreters remain subject to
the same module size, fuel, time, memory and permission limits as Rust/C/Go.

## C and Go

C implements `hydra_dispatch(method, request)` using length-delimited UTF-8 JSON
buffers. `hydra_host_call_json` returns an owned buffer; release it with `free`.
Check the JSON `error` envelope before consuming host results. Use a JSON library
for dynamic documents; never concatenate unescaped URLs or user input into JSON.

Go implements a `hydra.Handler`; export wrappers are in `go/example/main.go`.
`hydra.Host` marshals JSON and returns host errors. The host initializes WASI
reactors once per fresh instance. Guest memory is discarded after each call.

The wire protocol is shared with `hya-plugin-api`. See
`docs/design/PLUGINS.md` in the Hydra repository for plans and capabilities.
Settings schemas support `textbox`, `dropdown`, `number`, `radio`, `checkbox`,
and `boolean`, plus path and secret fields. Boolean and checkbox both store JSON
booleans; the GUI renders both as checkboxes using the application theme.

Optional lifecycle hooks are `enqueue(request)` (return `{allow: true}` or a
reasoned refusal), `process(request)` (write the supplied relative output path
through host data/exec calls, then return `{changed: true}`), and
`complete(request)` (notification). Declare each implemented hook in the manifest.
Processing needs `data`; the host supplies an isolated copy, bounds it by the
256 MiB data quota, validates the output, and replaces the completed file atomically.
`platform` returns the host OS/architecture. `program_install` needs `data`,
`exec_from_data`, a declared executable and HTTP grants; it accepts an HTTPS URL
and expected SHA256, pins the downloaded program, and refuses replacement of an
existing program. All these functions are accessible through `hydra.call`.

A resolver can return an ordered collection using `plan.entries`: each item has
`id`, `url`, and optional `title`. A collection has no `tracks`; the host validates
unique IDs, permitted HTTP(S) URLs, and a maximum of 512 items. Frontends resolve
each selected URL again when its job starts, so signed media URLs stay fresh.
The same preferences drive CLI quality flags, terminal choices and GUI controls.

Top-level manifest `welcome` is optional plain text. It is displayed after a
successful installation and retained in plugin info, making prerequisite setup
instructions available without requiring a first-run prompt.
