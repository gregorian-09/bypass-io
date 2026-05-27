ARG UBUNTU_VERSION=24.04
FROM ubuntu:${UBUNTU_VERSION}

ARG RUST_TOOLCHAIN=stable
ARG SPDK_REF=master
ARG SPDK_JOBS=2

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:${PATH}
ENV SPDK_ROOT=/opt/spdk
ENV PKG_CONFIG_PATH=/opt/spdk/build/lib/pkgconfig:${PKG_CONFIG_PATH}
ENV LD_LIBRARY_PATH=/opt/spdk/build/lib:${LD_LIBRARY_PATH}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        pkg-config \
        python3 \
        sudo \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rustup component add rustfmt clippy

RUN git clone --depth 1 --branch "${SPDK_REF}" https://github.com/spdk/spdk.git "${SPDK_ROOT}" \
    && cd "${SPDK_ROOT}" \
    && git submodule update --init --recursive \
    && ./scripts/pkgdep.sh \
    && ./configure \
    && make -j"${SPDK_JOBS}"

WORKDIR /workspace

CMD ["/bin/bash"]
