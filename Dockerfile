# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89

FROM rust:1.90-trixie@sha256:e227f20ec42af3ea9a3c9c1dd1b2012aa15f12279b5e9d5fb890ca1c2bb5726c AS builder

ARG CARGO_LEPTOS_VERSION=0.3.7
ENV RUSTUP_TOOLCHAIN=1.90.0
WORKDIR /build

RUN rustup target add --toolchain 1.90.0 wasm32-unknown-unknown \
    && cargo install cargo-leptos --locked --version "${CARGO_LEPTOS_VERSION}"

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY app ./app
COPY crates ./crates
COPY frontend ./frontend
COPY server ./server
COPY migrations ./migrations
COPY public ./public
COPY style ./style
COPY content ./content
COPY prompts ./prompts

RUN cargo leptos build --release \
        --bin-cargo-args=--locked \
        --lib-cargo-args=--locked \
    && test -x target/release/manchester-dnd-web \
    && test -s target/site/pkg/manchester-arcana.wasm \
    && install -d -m 0700 /build/runtime-data

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:d97bc0a941b8d4be647dc0ee75b264ddbb772f1ac5ba690a4309c00723b23775 AS runtime

ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="Manchester Arcana" \
    org.opencontainers.image.description="Private-MVP local-first Manchester-inspired tabletop game" \
    org.opencontainers.image.source="https://github.com/rdelacrz/manchester-dnd" \
    org.opencontainers.image.revision="$VCS_REF" \
    org.opencontainers.image.version="0.1.0-private-mvp" \
    org.opencontainers.image.licenses="LicenseRef-Manchester-Arcana-Private-Evaluation"

WORKDIR /app
COPY --from=builder --chown=65532:65532 /build/target/release/manchester-dnd-web /usr/local/bin/manchester-dnd-web
COPY --from=builder --chown=65532:65532 /build/target/site ./site
COPY --from=builder --chown=65532:65532 /build/content ./content
COPY --from=builder --chown=65532:65532 /build/prompts ./prompts
COPY --from=builder --chown=65532:65532 --chmod=0700 /build/runtime-data ./data

ENV APP_ACCESS_MODE=local \
    LEPTOS_ENV=PROD \
    LEPTOS_OUTPUT_NAME=manchester-arcana \
    LEPTOS_SITE_ADDR=127.0.0.1:6789 \
    LEPTOS_SITE_PKG_DIR=pkg \
    LEPTOS_SITE_ROOT=/app/site \
    CONTENT_PACK_ROOT=/app/content/packs \
    CONTENT_DEFAULT_THEME_PACK_ID=dev.manchester-arcana.rainbound-borough \
    EVENT_PROMPT_DIR=/app/prompts/events/private \
    TEXT_LLM_BACKEND=disabled \
    IMAGE_LLM_BACKEND=disabled \
    RUST_LOG=manchester_dnd=info,tower_http=info

USER 65532:65532
EXPOSE 6789
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/manchester-dnd-web"]
