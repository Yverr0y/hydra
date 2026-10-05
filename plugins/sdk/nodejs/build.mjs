#!/usr/bin/env node
import {spawnSync} from 'node:child_process';
import {copyFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
const [source, output = 'plugin.wasm'] = process.argv.slice(2);
if (!source) throw new Error('usage: node build.mjs plugin.js [plugin.wasm]');
const runtime = resolve(dirname(fileURLToPath(import.meta.url)), '../runtime');
const result = spawnSync('cargo', ['build', '--locked', '--manifest-path', resolve(runtime, 'Cargo.toml'), '--target-dir', resolve(runtime, 'target'), '--release', '--target', 'wasm32-wasip1', '--features', 'javascript'], {
  stdio: 'inherit', env: {...process.env, HYDRA_PLUGIN_SOURCE: resolve(source)}
});
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status || 1);
copyFileSync(resolve(runtime, 'target/wasm32-wasip1/release/hydra_script_guest.wasm'), resolve(output));
