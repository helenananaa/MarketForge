FROM node:22-bookworm-slim AS node
FROM python:3.12.11-slim-bookworm
ARG CODEX_VERSION=0.162.1
ARG OPENCODE_VERSION=1.19.0
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && apt-get update && apt-get install -y --no-install-recommends git curl ca-certificates procps \
 && rm -rf /var/lib/apt/lists/* \
 && npm install --global @openai/codex@${CODEX_VERSION} opencode-ai@${OPENCODE_VERSION} \
 && npm cache clean --force
RUN python -m pip install --no-cache-dir mcp==1.30.0 matplotlib==3.11.2
COPY python/marketforge /opt/marketforge/python/marketforge
COPY python/pyproject.toml /opt/marketforge/python/pyproject.toml
RUN python -m pip install --no-cache-dir --no-deps /opt/marketforge/python \
 && codex --version && opencode --version
ENV HOME=/work CODEX_HOME=/work/.codex PYTHONPATH=/work:/opt/marketforge/python \
    XDG_CONFIG_HOME=/work/.config XDG_DATA_HOME=/work/.local/share \
    XDG_STATE_HOME=/work/.local/state XDG_CACHE_HOME=/work/.cache
USER 65534:65534
WORKDIR /work
CMD ["python", "-c", "import time; time.sleep(10**9)"]
