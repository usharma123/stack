# Canonical fixture runtime: Python 3.13.16 + uv 0.12.23, digest-pinned. The venv lives
# outside /workspace so the worktree bind mount cannot hide it; the app code itself always
# comes from the mounted worktree (checked by its source token and code digest).
FROM ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 AS uv
FROM python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
COPY --from=uv /uv /uvx /usr/local/bin/
ENV UV_PROJECT_ENVIRONMENT=/opt/venv UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system \
    PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
WORKDIR /opt/rwb-build
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-install-project --python /usr/local/bin/python3.13
ENV PATH=/opt/venv/bin:$PATH
WORKDIR /workspace
CMD ["python", "-c", "import time; time.sleep(1000000000)"]
