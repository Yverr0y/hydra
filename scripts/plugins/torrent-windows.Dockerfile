FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl xz-utils git cmake make perl libboost-dev nlohmann-json3-dev \
    && rm -rf /var/lib/apt/lists/*
