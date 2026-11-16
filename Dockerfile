# syntax=docker/dockerfile:1
# Сборка ld2-server (PLAN-M12.1). Контекст — корень workspace.
# Собирается только пакет ld2-server: клиентские crate'ы в образ не попадают.
# Оба этапа — Alpine (musl): образ меньше, чем debian-based.
#
# Кеш-маунты BuildKit: target/ и реестр cargo живут вне слоёв и переживают
# повторные сборки — правки кода компилируются инкрементально, а не с нуля.
# Хранятся только локально в builder'е (в CI не попадают в --cache-to/from).
FROM rust:1-alpine AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked -p ld2-server && \
    # Бинарник выносим из кеш-маунта: тот размонтируется после RUN,
    # поэтому COPY --from=build /src/target/... работать не будет.
    cp target/release/ld2-server /tmp/ld2-server

FROM alpine:3.24
RUN apk add --no-cache ca-certificates
COPY --from=build /tmp/ld2-server /usr/local/bin/ld2-server
EXPOSE 24817
ENTRYPOINT ["ld2-server"]
# Конфиг монтируется compose-файлом (deploy/server.toml) или своим:
#   -v ./my-server.toml:/etc/ld2/server.toml:ro
CMD ["--config", "/etc/ld2/server.toml"]
