FROM rust:1.57 as builder
WORKDIR /usr/src/chrysanthemum
COPY . .
RUN cargo install --path .

FROM debian:buster-slim
COPY --from=builder /usr/local/cargo/bin/chrysanthemum /usr/local/bin/chrysanthemum
CMD ["chrysanthemum", "/var/chrysanthemum/chrysanthemum.cfg.json"]
