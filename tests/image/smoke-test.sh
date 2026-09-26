#!/usr/bin/env bash
# Usage: docker run --rm --volume "$PWD/tests/image:/tests:ro" ezra:dev bash /tests/smoke-test.sh

set -euo pipefail

failure_count=0

report_pass() {
    printf '  ok    %s\n' "$1"
}

report_failure() {
    printf '  FAIL  %s\n' "$1" >&2
    failure_count=$((failure_count + 1))
}

check_succeeds() {
    local description="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        report_pass "${description}"
    else
        report_failure "${description}"
    fi
}

check_equals() {
    local description="$1" expected="$2" actual="$3"
    if [[ ${actual} == "${expected}" ]]; then
        report_pass "${description}"
    else
        report_failure "${description}: expected '${expected}', got '${actual}'"
    fi
}

finish() {
    if ((failure_count > 0)); then
        echo "${failure_count} check(s) failed." >&2
        exit 1
    fi
    echo "All checks passed."
}

check_version_command() {
    local description="$1"
    shift
    local version_output
    if version_output="$("$@" 2>&1)"; then
        report_pass "${description}: $(head --lines=1 <<<"${version_output}")"
    else
        report_failure "${description}: '$*' failed: ${version_output}"
    fi
}

check_command_exists() {
    local command_name="$1"
    if command -v "${command_name}" >/dev/null; then
        report_pass "${command_name} is on PATH"
    else
        report_failure "${command_name} is missing from PATH"
    fi
}

echo "Commands on PATH:"
for command_name in bc dig file lsof nc ping ps pstree ss strace tini tree unzip xxd zip; do
    check_command_exists "${command_name}"
done

echo "Core tools:"
check_version_command "bash" bash --version
check_version_command "curl" curl --version
check_version_command "wget" wget --version
check_version_command "rsync" rsync --version
check_version_command "openssl" openssl version
check_version_command "gpg" gpg --version
check_version_command "ssh" ssh -V
check_version_command "zstd" zstd --version
check_version_command "envsubst" envsubst --version
check_version_command "GNU time" /usr/bin/time --version
check_succeeds "awk is GNU awk" bash -c 'awk --version | grep --quiet "GNU Awk"'
check_succeeds "man pages are installed" bash -c 'man rsync | grep --quiet "^NAME"'
check_succeeds "man -k finds pages installed in the image" bash -c 'man -k "^rsync$" | grep --quiet rsync'
check_succeeds "man-db skips index updates on later installs" test ! -e /var/lib/man-db/auto-update
check_succeeds "git help shows the manual" bash -c 'git commit --help | grep --quiet "git-commit - Record changes"'

echo "Developer tools:"
check_version_command "git" git --version
check_version_command "ripgrep" rg --version
check_version_command "fd" fd --version
check_version_command "shellcheck" shellcheck --version
check_version_command "sqlite3" sqlite3 --version
check_version_command "nano" nano --version

echo "Build toolchain:"
check_version_command "gcc" gcc --version
check_version_command "g++" g++ --version
check_version_command "make" make --version
check_version_command "cmake" cmake --version
check_version_command "ninja" ninja --version
check_version_command "autoconf" autoconf --version
check_version_command "automake" automake --version
check_version_command "libtoolize" libtoolize --version
check_version_command "pkg-config" pkg-config --version
check_succeeds "pkg-config finds openssl" pkg-config --exists openssl
check_succeeds "pkg-config finds libffi" pkg-config --exists libffi

echo "Python:"
check_version_command "python" python --version
check_version_command "python3" python3 --version
python_minor_version="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null || echo "unknown")"
check_succeeds "pkg-config finds the python${python_minor_version} headers" pkg-config --exists "python-${python_minor_version}"
python_venv_directory="$(mktemp --directory)"
check_succeeds "python3 -m venv provides pip" bash -c "python3 -m venv '${python_venv_directory}/venv' && '${python_venv_directory}/venv/bin/pip' --version"
rm --recursive --force "${python_venv_directory}"

echo "mise system tools:"
check_version_command "node" node --version
check_version_command "npm" npm --version
check_version_command "npx" npx --version
check_version_command "pnpm" pnpm --version
check_version_command "corepack" corepack --version
check_version_command "gh" gh --version
check_version_command "yq" yq --version
check_version_command "uv" uv --version
check_version_command "jq" jq --version
check_version_command "git-lfs" git lfs version
check_version_command "protoc" protoc --version
check_version_command "yarn" yarn --version
check_succeeds "pnpm is the pinned pnpm, not corepack" grep --quiet "/installs/pnpm/" /usr/local/bin/pnpm
check_equals "node runs through the mise shims" "/usr/local/share/ezra/shims/node" "$(command -v node)"
check_equals "git-lfs bypasses mise" "/usr/local/bin/git-lfs" "$(command -v git-lfs)"
check_succeeds "pnpx is not on PATH" bash -c '! command -v pnpx'
check_succeeds "install.sh is not on PATH" bash -c '! command -v install.sh'
check_succeeds "git-lfs filters registered system-wide" git config --system --get filter.lfs.process

project_directory="$(mktemp --directory --tmpdir=/projects)"
printf '[env]\nEXAMPLE = "1"\n' >"${project_directory}/mise.toml"
check_succeeds "tools work inside a project with its own mise.toml" \
    bash -c "cd '${project_directory}' && node --version && pnpm --version && jq --version && git lfs version"
rm --recursive --force "${project_directory}"

protoc_work_directory="$(mktemp --directory)"
cat >"${protoc_work_directory}/event.proto" <<'PROTO'
syntax = "proto3";
import "google/protobuf/timestamp.proto";
message Event { google.protobuf.Timestamp occurred_at = 1; }
PROTO
check_succeeds "protoc resolves well-known types" \
    protoc --proto_path="${protoc_work_directory}" --descriptor_set_out=/dev/null "${protoc_work_directory}/event.proto"
rm --recursive --force "${protoc_work_directory}"

echo "Chromium libraries:"
for shared_library in libnss3.so libgbm.so.1 libasound.so.2 libatk-bridge-2.0.so.0 libxkbcommon.so.0; do
    check_succeeds "${shared_library}" bash -c "ldconfig --print-cache | grep --quiet '${shared_library}'"
done

echo "Agent user:"
check_equals "runs as dev" "1000:1000:dev" "$(id --user):$(id --group):$(id --user --name)"
check_succeeds "the ubuntu user is gone" bash -c '! getent passwd ubuntu'
tests_directory="$(dirname "${BASH_SOURCE[0]}")"
files_owned_by_agent_elsewhere="$(find / -xdev \( -path /proc -o -path /tmp -o -path /home/dev -o -path /config -o -path /projects -o -path "${tests_directory}" \) -prune -o -uid 1000 -print 2>/dev/null || true)"
check_succeeds "dev cannot add setup scripts" bash -c '! touch /etc/ezra/setup.d/probe'
check_equals "nothing outside /home/dev, /config and /projects is owned by uid 1000" "" "${files_owned_by_agent_elsewhere}"

finish
