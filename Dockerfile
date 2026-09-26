# syntax=docker/dockerfile:1

# Optional: --secret id=github_token,env=GITHUB_TOKEN avoids GitHub rate limits during mise installs.

# Must match the rust version in mise.toml.
ARG RUST_VERSION=1.98.1

FROM rust:${RUST_VERSION}-slim-trixie AS agent-box-build

ARG TARGETARCH
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
WORKDIR /src

RUN rustup target add x86_64-unknown-linux-musl

RUN --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=bind,source=crates,target=crates \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    <<'EOF'
set -euo pipefail
if [[ ${TARGETARCH} != amd64 ]]; then
    echo "agent-box is only built for amd64 so far, not ${TARGETARCH}" >&2
    exit 1
fi
cargo build --locked --release --target x86_64-unknown-linux-musl --package agent-box
install -D target/x86_64-unknown-linux-musl/release/agent-box /out/agent-box
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

RUN <<'EOF'
set -euo pipefail
userdel --remove ubuntu
if getent passwd 1000 >/dev/null || getent group 1000 >/dev/null; then
    echo "UID or GID 1000 is still taken after removing the ubuntu user" >&2
    exit 1
fi
groupadd --gid 1000 dev
useradd --uid 1000 --gid dev --create-home --shell /bin/bash dev
install --directory --owner=dev --group=dev /config /projects
EOF

COPY --from=agent-box-build /out/agent-box /usr/local/bin/agent-box

ENTRYPOINT ["/usr/bin/tini", "-s", "--", "/usr/local/bin/agent-box", "init", "--"]
CMD ["bash"]
