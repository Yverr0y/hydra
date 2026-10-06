#!/usr/bin/env bash
set -euo pipefail

# Docker must not create the Cargo target directory as root.
mkdir -p target

# Match the oldest desktop bundle's glibc floor, including the AppImage.
docker run --rm -v "$PWD:/source" -w /source ubuntu:20.04 bash -euc '
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends build-essential g++-10 git ca-certificates python3-pip libboost-dev libssl-dev
  python3 -m pip install cmake==3.31.6
  git clone --depth 1 --branch v3.11.3 https://github.com/nlohmann/json.git /tmp/json
  cmake -S /tmp/json -B /tmp/json-build -DJSON_BuildTests=OFF
  cmake --install /tmp/json-build
  # GCC 9 rejects the defaulted noexcept move assignment in libtorrent.
  cmake -S plugins/hydra-torrent/native -B target/torrent-native-static -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_COMPILER=g++-10 -DHYDRA_STATIC_ENGINE=ON
  cmake --build target/torrent-native-static --parallel 4
  chown -R "'"$(id -u):$(id -g)"'" target/torrent-native-static
'
