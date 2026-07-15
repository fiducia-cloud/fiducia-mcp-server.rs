# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89

# Build context is this repository. The sibling client path dependency is
# fetched at an immutable commit so standalone builds remain reproducible and
# do not depend on a moving local checkout.
FROM rust:1.97.0-slim-bookworm@sha256:6d220bf85c74e842a79da63997af8d2e74455c0b8847d8bb3a5888572334991d AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates
WORKDIR /workspace
ARG INTERFACES_REF=67d1b4c33bcbafc7a027830e4da3afa1a1a0f137
ARG CLIENTS_REF=1ef817529e13613c1df60396daecc8854450712e
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
# upstream checksum before it enters the runtime image.
FROM debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df AS kubectl
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
      "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${TARGETARCH}/kubectl" \
      --output /tmp/kubectl \
    && echo "$checksum  /tmp/kubectl" | sha256sum --check \
    && chmod 0755 /tmp/kubectl

# The MCP server shells out to kubectl for its read-only Kubernetes tools, so
# this is an explicit non-root tool-runner rather than a distroless service.
FROM debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df
LABEL org.fiducia.runtime-profile="tool-runner-nonroot"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && groupadd --gid 65532 nonroot \
    && useradd --uid 65532 --gid 65532 --home-dir /home/nonroot --create-home \
      --shell /usr/sbin/nologin nonroot
COPY --from=kubectl --chown=65532:65532 /tmp/kubectl /usr/local/bin/kubectl
COPY --from=build --chown=65532:65532 /workspace/fiducia-mcp-server.rs/target/release/fiducia-mcp /usr/local/bin/fiducia-mcp
ENV HOME=/home/nonroot
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/fiducia-mcp"]
