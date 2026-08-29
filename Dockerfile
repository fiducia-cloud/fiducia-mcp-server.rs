# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89

# Build context is this repository. The sibling client path dependency is
# fetched at an immutable commit so standalone builds remain reproducible and
# do not depend on a moving local checkout.
FROM rust:1.97.1-slim-bookworm@sha256:2775a09d208ff0d7c1f50490c45b62db929e87ba1dcbc3f2132ac71a704bcdd3 AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates
WORKDIR /workspace
ARG INTERFACES_REF=2c5c806174e067fbe83ad48b724366323ba390a2
ARG CLIENTS_REF=5cd1a537f7ab98808ece4cdd09723be0bf49ce8b
RUN git init fiducia-interfaces \
    && git -C fiducia-interfaces remote add origin https://github.com/fiducia-cloud/fiducia-interfaces.git \
    && git -C fiducia-interfaces fetch --depth 1 origin "$INTERFACES_REF" \
    && git -C fiducia-interfaces checkout --detach FETCH_HEAD \
    && test "$(git -C fiducia-interfaces rev-parse HEAD)" = "$INTERFACES_REF"
RUN git init fiducia-clients \
    && git -C fiducia-clients remote add origin https://github.com/fiducia-cloud/fiducia-clients.git \
    && git -C fiducia-clients fetch --depth 1 origin "$CLIENTS_REF" \
    && git -C fiducia-clients checkout --detach FETCH_HEAD \
    && test "$(git -C fiducia-clients rev-parse HEAD)" = "$CLIENTS_REF"
COPY . fiducia-mcp-server.rs/
RUN cargo build --release --locked --manifest-path fiducia-mcp-server.rs/Cargo.toml \
    && strip fiducia-mcp-server.rs/target/release/fiducia-mcp

# Fetch kubectl in a disposable stage and verify the architecture-specific
# upstream checksum before it enters the runtime image. Transient transport
# failures are retried within fixed time bounds; integrity still depends on the
# reviewed per-architecture SHA-256 value, never on a successful download alone.
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS kubectl
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl
ARG TARGETARCH
ARG KUBECTL_VERSION=v1.34.1
RUN case "$TARGETARCH" in \
      amd64) checksum=7721f265e18709862655affba5343e85e1980639395d5754473dafaadcaa69e3 ;; \
      arm64) checksum=420e6110e3ba7ee5a3927b5af868d18df17aae36b720529ffa4e9e945aa95450 ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl --fail --location --silent --show-error \
      --proto '=https' \
      --tlsv1.2 \
      --connect-timeout 10 \
      --max-time 120 \
      --retry 5 \
      --retry-all-errors \
      --retry-connrefused \
      --retry-delay 2 \
      --retry-max-time 120 \
      "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${TARGETARCH}/kubectl" \
      --output /tmp/kubectl \
    && echo "$checksum  /tmp/kubectl" | sha256sum --check \
    && chmod 0755 /tmp/kubectl

# The MCP server shells out to kubectl for its read-only Kubernetes tools, so
# this is an explicit non-root tool-runner rather than a distroless service.
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241
LABEL org.fiducia.runtime-profile="tool-runner-nonroot"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && groupadd --gid 65532 nonroot \
    && useradd --uid 65532 --gid 65532 --home-dir /home/nonroot --create-home \
      --shell /usr/sbin/nologin nonroot
COPY --from=kubectl --chown=65532:65532 /tmp/kubectl /usr/local/bin/kubectl
COPY --from=build --chown=65532:65532 /workspace/fiducia-mcp-server.rs/target/release/fiducia-mcp /usr/local/bin/fiducia-mcp
COPY --from=build --chown=65532:65532 /workspace/fiducia-mcp-server.rs/.cli-flags.toml /usr/local/share/fiducia-mcp-server/.cli-flags.toml
ENV HOME=/home/nonroot
USER 65532:65532

# --- sops: decrypt at `docker run`, never at `docker build` ------------------
# The image carries only CIPHERTEXT (env/enc/<SOPS_ENV>.env.enc) and the sops
# binary. The age key arrives at run time (SOPS_AGE_KEY / SOPS_AGE_KEY_FILE);
# scripts/sops-entrypoint.sh decrypts into the process environment and execs
# the real command, so no plaintext ever lands in a layer or on disk.
# See env/README.md.
ARG SOPS_ENV=local
COPY --chmod=0755 --from=ghcr.io/getsops/sops:v3.10.2-alpine /usr/local/bin/sops /usr/local/bin/sops
COPY --chmod=0755 scripts/sops-entrypoint.sh /usr/local/bin/sops-entrypoint.sh
COPY --chmod=0644 env/enc/${SOPS_ENV}.env.enc /app/secrets/app.env
ENV SOPS_SECRETS_FILE=/app/secrets/app.env

# ores-otel: in-process OTLP to the cluster collector. The *-sidecar.rs image is a separate loopback helper on 127.0.0.1:9090 — do not EXPOSE 4317/4318 or 9090.
ENV OTEL_SERVICE_NAME=fiducia-mcp \
    OTEL_EXPORTER_OTLP_ENDPOINT=http://dd-otel-collector.observability.svc.cluster.local:4317 \
    RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/sops-entrypoint.sh", "/usr/local/bin/fiducia-mcp"]
