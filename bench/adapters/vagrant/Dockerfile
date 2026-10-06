# Pinned application toolchain (Python 3.13 + uv), image references from images.lock.json.
# Dependencies are installed by `uv sync --frozen` into the bind-mounted checkout.
ARG PYTHON_IMAGE
ARG UV_IMAGE
FROM ${UV_IMAGE} AS uvbin
FROM ${PYTHON_IMAGE}
COPY --from=uvbin /uv /uvx /usr/local/bin/
ENV UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system
CMD ["sleep", "infinity"]
