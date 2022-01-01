FROM rust:1.57 as builder
WORKDIR /usr/src/chrysanthemum
COPY ./src/ ./src/
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

RUN cargo install --path .

FROM debian:buster-slim
RUN apt-get update && apt-get install -y libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/chrysanthemum /usr/local/bin/chrysanthemum
CMD ["chrysanthemum", "/var/chrysanthemum/chrysanthemum.cfg.yml"]
