# CITAR server image.
#
# Two stages. The builder installs into a virtual environment; the runtime copies that environment
# and nothing else, so no compiler, no build cache and no package index metadata reach the final
# image. argon2-cffi is the reason a builder is needed at all: on a platform with no wheel it
# compiles, and a build toolchain in a production image is both weight and attack surface.
#
# The package comes from a wheel in wheelhouse/, never from the source: it carries the Rust engine
# (citar._engine), and building that here would put a Rust toolchain and a long compile into every
# image build. CI's package job builds the wheel (manylinux_2_28, the release profile, no test
# operations) and its docker job copies it there; a tests run keeps it as the artifact
# `wheel-manylinux`. To build one yourself, use maturin's manylinux container, since a wheel built
# on a newer Linux than this image's Debian 12 needs a newer glibc than the image has:
#
#   docker run --rm -v "$PWD":/io ghcr.io/pyo3/maturin build --release --out wheelhouse
#   docker build -t citar .
#   docker run -p 8765:8765 -v citar-state:/var/lib/citar citar
#
# For a public deployment with TLS, see docker-compose.yml, which puts a reverse proxy in front.

# --------------------------------------------------------------------------------- builder
FROM python:3.12-slim-bookworm AS builder

# build-essential and libffi are for any dependency without a prebuilt wheel for this platform.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential libffi-dev \
 && rm -rf /var/lib/apt/lists/*

ENV PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PIP_NO_CACHE_DIR=1

RUN python -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"

WORKDIR /src
RUN pip install --upgrade pip setuptools wheel

# Dependencies first, from the project metadata alone: the [server] extra's requirements, which
# scripts/ci/requirements.py reads out of pyproject.toml. This layer is rebuilt only when the
# dependency list changes, so a new wheel does not re-download the world. `citar` is installed
# with --no-deps afterwards, over the top of the environment this produced.
COPY pyproject.toml ./
COPY scripts/ci/requirements.py ./scripts/ci/requirements.py
RUN python scripts/ci/requirements.py server > /tmp/requirements.txt \
 && pip install -r /tmp/requirements.txt

# The wheel. `wheelhouse*` rather than `wheelhouse/`: a COPY whose only source is missing fails
# with Docker's own message, and this one says what to do. With several wheels there (another
# platform's, an older build), pip takes the newest that installs on this one.
COPY pyproject.toml wheelhouse* /tmp/wheelhouse/
RUN ls /tmp/wheelhouse/*.whl >/dev/null 2>&1 || { \
      echo "No wheel in wheelhouse/: build one first (see the top of the Dockerfile)." >&2; exit 1; } \
 && pip install --no-deps --no-index --find-links /tmp/wheelhouse citar \
 && python -c "import citar._engine"

# --------------------------------------------------------------------------------- runtime
FROM python:3.12-slim-bookworm AS runtime

LABEL org.opencontainers.image.title="CITAR" \
      org.opencontainers.image.description="Civ Inspired Tool for AI Research - a Civilization V-style 4X game for benchmarking language models" \
      org.opencontainers.image.source="https://github.com/jprodgers/CITAR" \
      org.opencontainers.image.licenses="MPL-2.0"

# curl is here for the health check below and nothing else; it is 400 KB and saves shipping a
# Python health-check script that would need the app's own environment to run.
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl \
 && rm -rf /var/lib/apt/lists/*

# A non-root user with a fixed uid, so a bind-mounted host directory has predictable ownership.
RUN useradd --system --uid 10001 --home /var/lib/citar --shell /usr/sbin/nologin citar \
 && mkdir -p /var/lib/citar \
 && chown citar:citar /var/lib/citar

COPY --from=builder /opt/venv /opt/venv

ENV PATH="/opt/venv/bin:$PATH" \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    # Everything CITAR writes goes here: saves, the database, benchmark runs, reports. It is the
    # only path that needs a volume, and the only one that needs backing up.
    CITAR_STATE_DIR=/var/lib/citar \
    # Containers are reached from outside themselves, so loopback would make the server
    # unreachable however the ports were published.
    CITAR_HOST=0.0.0.0 \
    CITAR_PORT=8765

VOLUME ["/var/lib/citar"]
EXPOSE 8765
USER citar
WORKDIR /var/lib/citar

# Asks the app a question only a working app can answer: this route reads the settings and the
# database. A TCP check would pass while the server was returning 500 to everything.
HEALTHCHECK --interval=30s --timeout=5s --start-period=40s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8765/api/auth/config >/dev/null || exit 1

ENTRYPOINT ["citar"]
CMD ["serve"]
