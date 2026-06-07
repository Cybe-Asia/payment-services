FROM rust:1.91.1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml .
RUN mkdir src && echo "fn main() { println!(\"dummy\"); }" > src/main.rs
RUN cargo build --release || true
# Remove dummy build artifacts so the second build actually picks up the real source.
RUN rm -rf src target/release/deps/payment_service* target/release/payment-service* target/release/.fingerprint/payment_service* || true

COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/payment-service /app/payment-service
ENV SERVER_PORT=8085
EXPOSE 8085
CMD ["/app/payment-service"]
