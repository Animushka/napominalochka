# syntax=docker/dockerfile:1

FROM rust:1-alpine AS dependencies
RUN apk add --no-cache musl-dev
WORKDIR /src

# Build dependencies in their own layer so source changes don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

FROM dependencies AS build
COPY migrations ./migrations
COPY locales ./locales
COPY web ./web
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM alpine:3
RUN apk add --no-cache ca-certificates \
    && adduser -D -H -u 10001 bot \
    && mkdir -p /app/data \
    && chown bot:bot /app/data
WORKDIR /app
COPY --from=build /src/target/release/extremely-naggy-bot /usr/local/bin/extremely-naggy-bot
USER bot
VOLUME ["/app/data"]
# Mini App web server (see WEB_APP_URL).
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s \
    CMD wget -q -O /dev/null http://127.0.0.1:8080/health || exit 1
ENTRYPOINT ["/usr/local/bin/extremely-naggy-bot"]
