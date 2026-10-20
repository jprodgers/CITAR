# CITAR server image (design/phase5-release.md P5.7).
#
# Built from a release wheel, never from the source: the wheel carries the Rust engine (citar._engine), and building
# it here would put a Rust toolchain and a long compile into every image build. package.yml's image job builds one
# image per architecture, natively on a runner of that architecture (linux/amd64 and linux/arm64, no emulation), each
# from its platform's wheel (manylinux_2_28, the release profile, no test operations) in wheelhouse/ and with the
# release's tested dependency set (citar-<version>-server-constraints.txt) copied to wheelhouse/constraints.txt; it
# runs and checks each image (scripts/ci/check_image.py) and, on a release's dry run and on a tag, saves it for
# release.yml to push, so the image pushed is the image tested.
#
# To build one yourself, put a Linux wheel of CITAR in wheelhouse/ (a release's, or one built in maturin's manylinux
# container: a wheel built on a newer Linux than its tag says needs a newer glibc than an older system has) and, if you
# have them, the release's constraints:
#
#   mkdir -p wheelhouse && cp citar-*-manylinux_2_28_x86_64.whl wheelhouse/
#   cp citar-*-server-constraints.txt wheelhouse/constraints.txt     # optional: the versions the release was tested with
#   docker build -t citar .
#   docker run -p 127.0.0.1:8765:8765 -v citar-state:/var/lib/citar citar
#
# For a public deployment with TLS, see docker-compose.yml, which puts a reverse proxy in front.
#
# Everything is installed as a wheel, the dependencies (--only-binary=:all:) as well as CITAR, so nothing compiles: the
# image needs no compiler and nothing from Debian's archive, and its system packages are the base image's. The base is
# pinned by the digest of its multi-architecture index, one digest for both architectures and both stages (a test
# holds them equal): python:3.12-slim-trixie (Debian 13, glibc 2.41; Python 3.12, the VPS's) as Docker Hub served it
# on 2026-10-20, Python 3.12.15 built 2026-10-06. Dependabot's docker updates move the digest, and .github/
# dependabot.yml keeps the Python at 3.12.

# --------------------------------------------------------------------------------- builder
FROM python:3.12-slim-trixie@sha256:a6e34c598f2467ed0e9a8d349809fcd8b5c603269512df273a0bb1784edc11b1 AS builder

ENV PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_NO_INPUT=1

RUN python -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH"

WORKDIR /src
COPY pyproject.toml ./
COPY scripts/ci/requirements.py ./scripts/ci/requirements.py

# The wheel, and the constraints when there are any. `wheelhouse*` rather than `wheelhouse/`: a COPY whose only source
# is missing fails with the engine's own message, and the check below says what to do.
COPY pyproject.toml wheelhouse* /tmp/wheelhouse/

# The [server] extra's requirements (scripts/ci/requirements.py reads them out of pyproject.toml), wheels only, at the
# release's tested versions when wheelhouse/constraints.txt is there; then CITAR from its wheel and nothing else from
# an index (with several wheels in wheelhouse/, another architecture's or an older build, pip takes the newest that
# installs here); then pip's own check that every requirement of what was installed holds.
RUN set -eu; \
    ls /tmp/wheelhouse/*.whl >/dev/null 2>&1 || { \
      echo "No wheel in wheelhouse/: put a Linux wheel of CITAR there first (see the top of the Dockerfile)." >&2; \
      exit 1; }; \
    python scripts/ci/requirements.py server > /tmp/requirements.txt; \
    if [ -f /tmp/wheelhouse/constraints.txt ]; then \
      echo "the dependencies at the release's tested versions (wheelhouse/constraints.txt)"; \
      set -- --constraint /tmp/wheelhouse/constraints.txt; \
    else \
      echo "no wheelhouse/constraints.txt: the dependencies' newest wheels within pyproject.toml's ranges"; \
      set --; \
    fi; \
    pip install --only-binary=:all: "$@" -r /tmp/requirements.txt; \
    pip install --only-binary=:all: --no-deps --no-index --find-links /tmp/wheelhouse citar; \
    pip check; \
    python -P -c "import citar._engine"

# --------------------------------------------------------------------------------- runtime
FROM python:3.12-slim-trixie@sha256:a6e34c598f2467ed0e9a8d349809fcd8b5c603269512df273a0bb1784edc11b1 AS runtime

LABEL org.opencontainers.image.title="CITAR" \
      org.opencontainers.image.description="Civ Inspired Tool for AI Research - a Civilization V-style 4X game for benchmarking language models" \
      org.opencontainers.image.source="https://github.com/jprodgers/CITAR" \
      org.opencontainers.image.licenses="MPL-2.0"

# A non-root user and group with fixed ids, so a bind-mounted host directory has predictable ownership. The state
# directory is the server's alone (0750, as on a server's release layout).
RUN groupadd --system --gid 10001 citar \
 && useradd --system --uid 10001 --gid citar --home-dir /var/lib/citar --no-create-home \
      --shell /usr/sbin/nologin citar \
 && mkdir -p /var/lib/citar \
 && chown citar:citar /var/lib/citar \
 && chmod 0750 /var/lib/citar

# The venv and nothing else from the builder: no pip cache, no wheelhouse, no requirements file. It stays root's, so
# the server can read its code and never change it.
COPY --from=builder /opt/venv /opt/venv

# CITAR_STATE_DIR: everything CITAR writes goes there (saves, the database, benchmark runs, reports); it is the only
# path that needs a volume, and the only one that needs backing up. CITAR_HOST: a container is reached from outside
# itself, so loopback would make the server unreachable however its port was published.
ENV PATH="/opt/venv/bin:$PATH" \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    CITAR_STATE_DIR=/var/lib/citar \
    CITAR_HOST=0.0.0.0 \
    CITAR_PORT=8765

VOLUME ["/var/lib/citar"]
EXPOSE 8765
# By number, so a platform that only runs images it can tell are not root's (Kubernetes' runAsNonRoot) can tell.
USER 10001:10001
WORKDIR /var/lib/citar

# Asks the app a question only a working app can answer: this route reads the settings and the database, where a TCP
# check would pass while the server answered 500 to everything. Run by the server's own Python with -I, so nothing in
# the working directory (the state directory) and no PYTHON* variable can stand in for the standard library, on the
# port the server listens on.
HEALTHCHECK --interval=30s --timeout=5s --start-period=40s --retries=3 \
  CMD ["python", "-I", "-c", "import os, urllib.request; urllib.request.urlopen('http://127.0.0.1:%s/api/auth/config' % os.environ.get('CITAR_PORT', '8765'), timeout=4)"]

ENTRYPOINT ["citar"]
CMD ["serve"]
