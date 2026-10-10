FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web
COPY hist-group.jpg ./hist-group.jpg
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home hist
COPY --from=builder /build/target/release/hist-auto-ckeckin /usr/local/bin/hist-auto-ckeckin
USER hist
EXPOSE 38080
ENTRYPOINT ["hist-auto-ckeckin"]
