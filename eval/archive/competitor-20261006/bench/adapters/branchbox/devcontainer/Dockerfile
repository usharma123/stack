# Canonical fixture runtime: Python 3.13.16 + uv 0.12.23, digest-pinned. The locked venv is
# built into the image (outside /workspaces), so recreated containers never depend on the
# release's unchecked postCreateCommand; the app code comes from the mounted worktree.
FROM ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 AS uv
FROM python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641
COPY --from=uv /uv /uvx /usr/local/bin/
ENV UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system UV_PROJECT_ENVIRONMENT=/opt/rwb-venv \
    PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
WORKDIR /opt/rwb-build
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-install-project --python /usr/local/bin/python3.13
