# The keeper image: the release keeler binary, the proving keys it checks
# against lend/lend-keys.CHECKSUM and the swap example's swap-keys.CHECKSUM,
# and the devnet configs. Built by .github/workflows/image.yml from the
# directory it stages (keeler, keys/lend, keys/swap, config).
FROM debian:trixie-slim@sha256:8aa5a7bf5a1955bddbe7d74fcdd0b91ab30e74362a8b73af672b0481b35b44a0

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --no-create-home --home-dir /data keeler \
    && mkdir /data \
    && chown keeler:keeler /data

COPY keeler /usr/local/bin/keeler
COPY keys /opt/keeler/keys
COPY config /config

ENV KEELER_LEND_KEYS_DIR=/opt/keeler/keys/lend \
    SWAP_KEYS_DIR=/opt/keeler/keys/swap

USER keeler
WORKDIR /data
VOLUME /data
ENTRYPOINT ["/usr/local/bin/keeler"]
