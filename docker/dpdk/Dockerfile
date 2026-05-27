ARG UBUNTU_VERSION=24.04
FROM ubuntu:${UBUNTU_VERSION}

ARG RUST_TOOLCHAIN=stable
ARG DPDK_REF=main
ARG DPDK_JOBS=2

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:${PATH}
ENV DPDK_ROOT=/opt/dpdk
ENV PKG_CONFIG_PATH=/opt/dpdk/build/meson-uninstalled:${PKG_CONFIG_PATH}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        libnuma-dev \
        meson \
        ninja-build \
        pkg-config \
        python3-pyelftools \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rustup component add rustfmt clippy

RUN git clone --depth 1 --branch "${DPDK_REF}" https://github.com/DPDK/dpdk.git "${DPDK_ROOT}" \
    && cd "${DPDK_ROOT}" \
    && meson setup build \
    && ninja -C build -j"${DPDK_JOBS}"

WORKDIR /workspace

CMD ["/bin/bash"]
