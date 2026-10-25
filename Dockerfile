FROM node:24.19.0-bookworm-slim@sha256:a9f5f7c91a432850b2a8a7797adf5eadb6c733ceed61167806cee7ea7fbc29df AS web
WORKDIR /build/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --ignore-scripts
COPY web/ ./
RUN npm run typecheck && npm run build && npm run check:bundle

FROM rust:1.99.1-slim-bookworm@sha256:86234e857a413004348b471d11a4ebe1aa92d2eae2b6b143cac68fd1ac3c48be AS rust
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY schemas/ ./schemas/
COPY --from=web /build/web/dist ./web/dist
RUN cargo build --release --locked -p calendar-server --bin calendar-server --no-default-features --features production-web \
    && strip target/release/calendar-server \
    && mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=rust /build/target/release/calendar-server /usr/local/bin/calrelay
COPY --from=rust --chown=65532:65532 /data /data
USER 65532:65532
WORKDIR /data
ENV CALRELAY_DATABASE=/data/calrelay.db
STOPSIGNAL SIGTERM
VOLUME ["/data"]
HEALTHCHECK --interval=15s --timeout=3s --start-period=60s --retries=3 \
    CMD ["/usr/local/bin/calrelay", "healthcheck", "live", "--deployment-config", "/run/calrelay/deployment.json", "--timeout-ms", "2000"]
ENTRYPOINT ["/usr/local/bin/calrelay"]
CMD ["--deployment-config", "/run/calrelay/deployment.json"]
