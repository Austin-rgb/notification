# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /src
# Cargo.lock is copied when present (commit it for reproducible builds).
COPY Cargo.toml Cargo.lock* build.rs ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --bin notification

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home notification
WORKDIR /app
COPY --from=build /src/target/release/notification /usr/local/bin/notification
# Email templates are loaded from ./templates relative to the working directory.
COPY templates ./templates
USER notification
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
  CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["notification"]
