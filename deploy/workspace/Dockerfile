FROM ghcr.io/coder/code-server@sha256:b9e8056bb74bd0c4f90284c7559eddeccd38400cfedc28e64c9d3ca50fef1f83
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-venv nodejs npm git curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --chmod=755 start.sh /usr/local/bin/cofleet-workspace-start
USER 1000:1000
ENV HOME=/home/coder
WORKDIR /home/coder
HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=3 CMD python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=2)"
ENTRYPOINT ["/usr/local/bin/cofleet-workspace-start"]
