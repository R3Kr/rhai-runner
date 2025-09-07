FROM rust:1.89-alpine3.22 AS builder
WORKDIR /app
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl

FROM alpine:3.22.1
WORKDIR /app
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/rhai-runner /app/rhai-runner
EXPOSE 3000
CMD ["/app/rhai-runner"]
