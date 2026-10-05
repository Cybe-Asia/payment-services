FROM rust:1.95-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
RUN cargo build --release --locked --bin payment-service
FROM debian:bookworm-slim
ARG SOURCE_REPOSITORY
ARG SOURCE_REVISION
ARG SOURCE_TREE_SHA256
LABEL org.opencontainers.image.source=$SOURCE_REPOSITORY \
      org.opencontainers.image.revision=$SOURCE_REVISION \
      tech.cybe.digital-school.source-tree-sha256=$SOURCE_TREE_SHA256
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/payment-service /app/payment-service
ENV SERVER_PORT=8085
EXPOSE 8085
CMD ["/app/payment-service"]
