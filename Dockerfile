FROM rust:1-slim-bookworm AS build
WORKDIR /src
# Dependencies first, so a change to the relay's own code does not rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/hue-relay /usr/local/bin/hue-relay
ENTRYPOINT ["/usr/local/bin/hue-relay"]
