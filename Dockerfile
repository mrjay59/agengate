# Stage 1: Build
FROM rust:1.75-bookworm AS builder

WORKDIR /app
COPY . .

RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/agentgate /app/agentgate

EXPOSE 8080

CMD ["/app/agentgate"]