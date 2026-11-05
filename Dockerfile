# Сборка ld2-server (PLAN-M12.1). Контекст — корень workspace.
# Собирается только пакет ld2-server: клиентские crate'ы в образ не попадают.
# Оба этапа — Alpine (musl): образ меньше, чем debian-based.
FROM rust:1-alpine AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release -p ld2-server

FROM alpine:3.24
RUN apk add --no-cache ca-certificates
COPY --from=build /src/target/release/ld2-server /usr/local/bin/ld2-server
EXPOSE 24817
ENTRYPOINT ["ld2-server"]
# Конфиг монтируется compose-файлом (deploy/server.toml) или своим:
#   -v ./my-server.toml:/etc/ld2/server.toml:ro
CMD ["--config", "/etc/ld2/server.toml"]
