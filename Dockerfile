FROM rust:1.83-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/issue-watch /usr/local/bin/issue-watch
COPY config.example.toml /app/config.example.toml
RUN mkdir -p /app/data
VOLUME ["/app/data"]
ENV ISSUE_WATCH_CONFIG=/app/config.toml
CMD ["issue-watch"]
