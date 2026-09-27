# syntax=docker/dockerfile:1

# Optional: --secret id=github_token,env=GITHUB_TOKEN avoids GitHub rate limits during mise installs.

# Must match the rust, node and pnpm versions in mise.toml.
ARG RUST_VERSION=1.98.1
ARG NODE_VERSION=26
ARG PNPM_VERSION=12.6.0

FROM node:${NODE_VERSION}-slim AS ezra-web

ARG PNPM_VERSION
WORKDIR /src/web
RUN npm install --global "pnpm@${PNPM_VERSION}"
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
COPY web/app/package.json app/
COPY web/client/package.json client/
COPY web/e2e/package.json e2e/
RUN --mount=type=cache,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile --filter "@ezra/app..."
COPY openapi.json /src/openapi.json
COPY web/client/openapi-ts.config.ts client/
COPY web/app app
RUN pnpm build

FROM rust:${RUST_VERSION}-slim-trixie AS ezra-build

ARG TARGETARCH
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
WORKDIR /src

RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked <<'EOF'
set -euo pipefail
apt-get update
apt-get install --yes --no-install-recommends musl-tools
rustup target add x86_64-unknown-linux-musl
EOF

RUN --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=bind,source=crates,target=crates \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    <<'EOF'
set -euo pipefail
if [[ ${TARGETARCH} != amd64 ]]; then
    echo "ezra is only built for amd64 so far, not ${TARGETARCH}" >&2
    exit 1
fi
cargo build --locked --release --target x86_64-unknown-linux-musl --package ezra
install -D target/x86_64-unknown-linux-musl/release/ezra /out/ezra
EOF

FROM ubuntu:26.04

ARG DEBIAN_FRONTEND=noninteractive
ENV LANG=C.UTF-8

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked <<'EOF'
set -euo pipefail
sed --in-place '\|^path-exclude=/usr/share/man/|d' /etc/dpkg/dpkg.cfg.d/excludes
rm --force /usr/bin/man
dpkg-divert --quiet --remove --rename /usr/bin/man
apt-get update
apt-get install --yes --no-install-recommends man-db
dpkg --search /usr/share/man/ \
    | sed 's|, |\n|g; s|: [^:]*$||' \
    | sort --unique \
    | xargs --no-run-if-empty apt-get install --reinstall --yes --no-install-recommends
EOF

RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked <<'EOF'
set -euo pipefail
apt-get update
apt-get install --yes --no-install-recommends \
    bash \
    bc \
    bind9-dnsutils \
    bsdextrautils \
    bzip2 \
    ca-certificates \
    curl \
    diffutils \
    file \
    findutils \
    gawk \
    gettext-base \
    gnupg \
    grep \
    gzip \
    iproute2 \
    iputils-ping \
    less \
    lsof \
    netcat-openbsd \
    openssh-client \
    openssl \
    patch \
    procps \
    psmisc \
    rsync \
    sed \
    strace \
    sudo-rs \
    tar \
    time \
    tini \
    tzdata \
    unzip \
    wget \
    xxd \
    xz-utils \
    zip \
    zstd
EOF

RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked <<'EOF'
set -euo pipefail
apt-get update
apt-get install --yes --no-install-recommends \
    fd-find \
    git \
    nano \
    ripgrep \
    shellcheck \
    sqlite3 \
    tree
ln --symbolic /usr/bin/fdfind /usr/local/bin/fd
EOF

RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked <<'EOF'
set -euo pipefail
apt-get update
apt-get install --yes --no-install-recommends \
    autoconf \
    automake \
    build-essential \
    cmake \
    libffi-dev \
    libssl-dev \
    libtool \
    ninja-build \
    pkgconf \
    python-is-python3 \
    python3 \
    python3-dev \
    python3-venv
EOF

ARG MISE_VERSION=2026.9.14
ARG MISE_MINISIGN_PUBLIC_KEY=RWTC3g8W3z4RZK3V3qv7fa1QY4JEWyBtqIHW+85QlJpZc5yG+uNYNBSZ
RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked <<'EOF'
set -euo pipefail
apt-get update
apt-get install --yes --no-install-recommends minisign

debian_architecture="$(dpkg --print-architecture)"
case "${debian_architecture}" in
    amd64) mise_architecture="x64" ;;
    arm64) mise_architecture="arm64" ;;
    *) echo "Unsupported architecture for mise: ${debian_architecture}" >&2; exit 1 ;;
esac

mise_release_url="https://github.com/jdx/mise/releases/download/v${MISE_VERSION}"
mise_asset_name="mise-v${MISE_VERSION}-linux-${mise_architecture}"
download_directory="$(mktemp --directory)"
cd "${download_directory}"

curl --fail --silent --show-error --location --remote-name-all \
    "${mise_release_url}/SHASUMS256.txt" \
    "${mise_release_url}/SHASUMS256.txt.minisig" \
    "${mise_release_url}/${mise_asset_name}"
minisign -V -m SHASUMS256.txt -P "${MISE_MINISIGN_PUBLIC_KEY}"
grep " ./${mise_asset_name}\$" SHASUMS256.txt | sha256sum --check --strict
install --mode=0755 "${mise_asset_name}" /usr/local/bin/mise

cd /
rm --recursive --force "${download_directory}"
apt-get purge --yes --auto-remove minisign
EOF

COPY image/mise/system-tools.toml /etc/mise/config.toml
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0

RUN --mount=type=secret,id=github_token,env=GITHUB_TOKEN \
    --mount=type=cache,target=/root/.cache/mise,sharing=locked \
    <<'EOF'
set -euo pipefail
mise install --system
install --directory /usr/local/share/ezra/shims
for tool_executable in node npm npx pnpm pn pnx corepack yarn yarnpkg gh yq uv uvx jq git-lfs protoc; do
    tool_path="$(mise which "${tool_executable}")"
    printf '#!/bin/sh\nexec %s "$@"\n' "'${tool_path}'" >"/usr/local/bin/${tool_executable}"
    chmod 0755 "/usr/local/bin/${tool_executable}"
    if [[ ${tool_executable} != git-lfs ]]; then
        ln --symbolic /usr/local/bin/mise "/usr/local/share/ezra/shims/${tool_executable}"
    fi
done
git lfs install --system --skip-repo
rm --recursive --force /root/.local/share/mise /root/.local/state/mise
EOF

ARG PLAYWRIGHT_VERSION=1.63.0
RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    --mount=type=cache,target=/root/.npm,sharing=locked \
    <<'EOF'
set -euo pipefail
npx --yes "playwright-core@${PLAYWRIGHT_VERSION}" install-deps chromium
EOF

RUN <<'EOF'
set -euo pipefail
rm --force /var/lib/man-db/auto-update
userdel --remove ubuntu
if getent passwd 1000 >/dev/null || getent group 1000 >/dev/null; then
    echo "UID or GID 1000 is still taken after removing the ubuntu user" >&2
    exit 1
fi
groupadd --gid 1000 dev
useradd --uid 1000 --gid dev --create-home --shell /bin/bash dev
install --directory --owner=dev --group=dev /config /home/dev/projects /cache
install --directory /etc/ezra/setup.d
EOF

RUN --mount=type=cache,target=/root/.npm,sharing=locked <<'EOF'
set -euo pipefail
browsers_directory="$(mktemp --directory)"
PLAYWRIGHT_BROWSERS_PATH="${browsers_directory}" npx --yes "playwright-core@${PLAYWRIGHT_VERSION}" install --no-shell chromium
mv "${browsers_directory}"/chromium-*/chrome-linux* /opt/chromium
install --directory --owner=dev --group=dev /home/dev/.cache /home/dev/.cache/ms-playwright
mv "${browsers_directory}"/ffmpeg-* /home/dev/.cache/ms-playwright/
chown --recursive dev:dev /home/dev/.cache/ms-playwright
rm --recursive --force "${browsers_directory}"
printf '#!/bin/sh\nexec /opt/chromium/chrome --no-sandbox --disable-dev-shm-usage "$@"\n' >/usr/local/bin/chromium
chmod 0755 /usr/local/bin/chromium
EOF

ENV MISE_DATA_DIR=/config/mise \
    MISE_CONFIG_DIR=/config/mise \
    MISE_STATE_DIR=/config/mise/state \
    MISE_TRUSTED_CONFIG_PATHS=/home/dev/projects \
    CLAUDE_CONFIG_DIR=/config/claude \
    CODEX_HOME=/config/codex \
    GH_CONFIG_DIR=/config/gh \
    GH_PATH=/usr/local/share/ezra/shims/gh \
    GIT_CONFIG_GLOBAL=/config/git/config \
    DISABLE_UPDATES=1 \
    PLAYWRIGHT_MCP_EXECUTABLE_PATH=/usr/local/bin/chromium \
    PATH=/home/dev/.local/bin:/config/mise/shims:/usr/local/share/ezra/shims:${PATH}

COPY image/codex/config.toml /etc/codex/config.toml
COPY --from=ezra-build /out/ezra /usr/local/bin/ezra
COPY --from=ezra-web /src/web/app/dist /usr/local/share/ezra/web

EXPOSE 8443
ENTRYPOINT ["/usr/bin/tini", "-s", "--", "/usr/local/bin/ezra", "init", "--"]
CMD ["ezra", "manager"]
