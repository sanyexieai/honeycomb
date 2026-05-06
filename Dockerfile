FROM rust:1.88-bookworm AS builder

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY apps ./apps
COPY crates ./crates

RUN cargo build --release -p hc-api

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /src/target/release/hc-api /usr/local/bin/hc-api
COPY .env.example /app/.env.example

ENV HC_API_BIND=0.0.0.0:8787
ENV HC_WORKSPACE_ROOT=/app/workspace
ENV HC_TENANT_ID=local
ENV HC_USER_ID=default

RUN mkdir -p /app/workspace

EXPOSE 8787
VOLUME ["/app/workspace"]

CMD ["hc-api"]
