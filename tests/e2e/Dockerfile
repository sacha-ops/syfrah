FROM rust:latest AS builder
WORKDIR /app
RUN rustup target add x86_64-unknown-linux-musl && \
    apt-get update && apt-get install -y musl-tools
COPY Cargo.toml Cargo.lock CLOUD_HYPERVISOR_VERSION CRUN_VERSION RUNSC_VERSION ZEROFS_VERSION ./
COPY layers layers
COPY bin bin
RUN cargo build --release --bin syfrah --target x86_64-unknown-linux-musl

# Build the fake cloud-hypervisor (Rust static binary, same musl target)
COPY tests/e2e/fake-ch tests/e2e/fake-ch
RUN cargo build --release --manifest-path tests/e2e/fake-ch/Cargo.toml --target x86_64-unknown-linux-musl

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    wireguard-tools \
    iproute2 \
    iputils-ping \
    procps \
    iptables \
    jq \
    curl \
    ncat \
    tini \
    crun \
    && rm -rf /var/lib/apt/lists/*

# Install gVisor (runsc) — used by the container runtime as the OCI runtime.
# If gVisor is unavailable at runtime (e.g. missing kernel support in nested
# Docker), the container runtime falls back to crun alone.
RUN curl -fsSL https://storage.googleapis.com/gvisor/releases/release/latest/x86_64/runsc \
        -o /usr/local/bin/runsc && \
    chmod +x /usr/local/bin/runsc

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/syfrah /usr/local/bin/syfrah
COPY --from=builder /app/tests/e2e/fake-ch/target/x86_64-unknown-linux-musl/release/fake-cloud-hypervisor /usr/local/lib/syfrah/cloud-hypervisor

# Download real VM images and kernel from the syfrah-images catalog.
# These are never actually booted — the fake cloud-hypervisor handles
# all CH API calls — but the preflight validator and image store require
# real files, and the disk clone step copies them into instance dirs.
ARG CATALOG_URL=https://github.com/sacha-ops/syfrah-images/releases/latest/download/catalog.json
RUN mkdir -p /opt/syfrah/images /run/syfrah/vms && \
    curl -fSL -o /tmp/catalog.json "$CATALOG_URL" && \
    BASE_URL=$(jq -r '.base_url' /tmp/catalog.json) && \
    KERNEL_FILE=$(jq -r '.kernel.file' /tmp/catalog.json) && \
    echo "Downloading kernel: ${BASE_URL}/${KERNEL_FILE}" && \
    curl -fSL -o /tmp/vmlinux.gz "${BASE_URL}/${KERNEL_FILE}" && \
    gunzip -c /tmp/vmlinux.gz > /opt/syfrah/vmlinux && \
    rm -f /tmp/vmlinux.gz && \
    for img in $(jq -r '.images[].name' /tmp/catalog.json); do \
        FILE=$(jq -r --arg n "$img" '.images[] | select(.name == $n) | .file' /tmp/catalog.json); \
        echo "Downloading image: ${BASE_URL}/${FILE}"; \
        curl -fSL -o "/tmp/${FILE}" "${BASE_URL}/${FILE}"; \
        gunzip -c "/tmp/${FILE}" > "/opt/syfrah/images/${img}.raw"; \
        rm -f "/tmp/${FILE}"; \
    done && \
    cp /tmp/catalog.json /opt/syfrah/catalog.json && \
    rm -f /tmp/catalog.json && \
    ls -lh /opt/syfrah/vmlinux /opt/syfrah/images/*.raw

CMD ["sleep", "infinity"]
