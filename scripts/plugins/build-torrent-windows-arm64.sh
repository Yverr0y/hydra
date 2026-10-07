#!/usr/bin/env bash
set -euo pipefail

exec bash "$(dirname "$0")/build-torrent-windows.sh" aarch64
