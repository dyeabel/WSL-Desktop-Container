# Vývojový obraz pro kompilaci a testy na Linuxu (spouští scripts/linux-test.sh).
FROM docker.io/library/rust:1-bookworm
RUN apt-get update \
    && apt-get install -y --no-install-recommends libgtk-3-dev libxdo-dev libayatana-appindicator3-dev \
    && rm -rf /var/lib/apt/lists/*
