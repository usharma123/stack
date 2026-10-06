# Benchmark glue (scripted): the worktree's own source is baked into its app image, so the
# app's SOURCE_TOKEN proves which checkout was built. Python/uv are digest-pinned images.
ARG RWB_UV_IMAGE
ARG RWB_PYTHON_IMAGE
FROM ${RWB_UV_IMAGE} AS uv
FROM ${RWB_PYTHON_IMAGE}
COPY --from=uv /uv /uvx /usr/local/bin/
ENV UV_PYTHON_DOWNLOADS=never UV_LINK_MODE=copy PYTHONDONTWRITEBYTECODE=1
WORKDIR /app
COPY pyproject.toml uv.lock ./
RUN uv sync --locked --no-install-project --python /usr/local/bin/python
COPY . /app
RUN uv sync --locked --python /usr/local/bin/python
CMD ["python", "-c", "import time; time.sleep(2**31)"]
