#!/usr/bin/env bash
# Runs the container tests against $EZRA_TEST_IMAGE as uid 1000, since they mount host folders that
# the container's dev user writes.
set -euo pipefail

getent passwd 1000 || sudo useradd --uid 1000 --create-home ezra-test
tester="$(getent passwd 1000 | cut -d: -f1)"
sudo usermod --append --groups docker "${tester}"

binaries="$(mktemp --directory)"
cargo test --locked --package ezra-container-tests --no-run --message-format=json \
    | jq -r 'select(.executable != null and .profile.test) | .executable' \
    | xargs install --mode=755 --target-directory="${binaries}"
chmod 755 "${binaries}"
for tests in "${binaries}"/*; do
    sudo --user="${tester}" --preserve-env=EZRA_TEST_IMAGE,MISE_GITHUB_TOKEN "${tests}" --ignored
done
