# syntax=docker/dockerfile:1

# Optional: --secret id=github_token,env=GITHUB_TOKEN avoids GitHub rate limits during mise installs.

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
for tool_executable in node npm npx pnpm pn pnx corepack yarn yarnpkg gh yq uv uvx jq git-lfs protoc; do
    tool_path="$(mise which "${tool_executable}")"
    printf '#!/bin/sh\nexec %s "$@"\n' "'${tool_path}'" >"/usr/local/bin/${tool_executable}"
    chmod 0755 "/usr/local/bin/${tool_executable}"
done
git lfs install --system --skip-repo
rm --recursive --force /root/.local/share/mise /root/.local/state/mise
EOF

ARG PLAYWRIGHT_VERSION=1
RUN --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    --mount=type=cache,target=/root/.npm,sharing=locked \
    <<'EOF'
set -euo pipefail
npx --yes "playwright@${PLAYWRIGHT_VERSION}" install-deps chromium
EOF
