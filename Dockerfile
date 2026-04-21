FROM rust:latest as builder
WORKDIR /app
COPY Cargo.toml .
RUN mkdir src && echo "fn main() {println!(\"build\");}" > src/main.rs
RUN cargo build --release || true
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/payment-service /app/payment-service
ENV SERVER_PORT=8085
EXPOSE 8085
CMD ["/app/payment-service"]
