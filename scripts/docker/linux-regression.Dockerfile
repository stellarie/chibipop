FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
ARG RUST_TOOLCHAIN=stable

# Ubuntu 24.04 is the release baseline. Keep this list explicit and minimal so
# an image rebuild records the exact distro package versions in its image ID.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cage \
        curl \
        dbus \
        dbus-user-session \
        fonts-noto-cjk \
        git \
        gzip \
        libfontconfig1 \
        libgl1 \
        libpipewire-0.3-0t64 \
        libwayland-client0 \
        pkg-config \
        procps \
        python3 \
        sway \
        tar \
        wayland-protocols \
        wl-clipboard \
        xdg-desktop-portal \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/cargo-home \
    RUSTUP_HOME=/rustup \
    PATH=/cargo-home/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rustup component add clippy

RUN useradd --create-home --uid 1000 --shell /bin/bash regression \
    && mkdir -p /work /cargo-home /cargo-target /runtime /smoke /artifacts \
    && chown -R regression:regression \
        /work /cargo-home /cargo-target /runtime /smoke /artifacts

USER regression
WORKDIR /work/source

CMD ["sleep", "infinity"]
