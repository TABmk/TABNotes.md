FROM rust:1.94-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY templates ./templates
COPY static ./static

RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && groupadd --system --gid 10001 tabnotes \
    && useradd --system --uid 10001 --gid tabnotes --home-dir /app --shell /usr/sbin/nologin tabnotes \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/tabnotes /usr/local/bin/tabnotes
COPY templates ./templates
COPY static ./static
RUN mkdir -p /app/data \
    && chown -R tabnotes:tabnotes /app

USER tabnotes

ENV BIND_ADDR=0.0.0.0:8080
ENV DATABASE_URL=sqlite://data/tabnotes.db

EXPOSE 8080

CMD ["tabnotes"]
