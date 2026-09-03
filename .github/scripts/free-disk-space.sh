#!/usr/bin/env bash
# Reclaim space on GitHub-hosted Ubuntu runners. A Thock build needs far more
# than the ~25 GB they ship with free; the toolchains removed here are not used
# by any Thock job.

set -euo pipefail

echo "Disk before:"
df -h /

sudo rm -rf \
    /usr/share/dotnet \
    /usr/share/swift \
    /usr/local/lib/android \
    /usr/local/.ghcup \
    /opt/ghc \
    /opt/hostedtoolcache/CodeQL \
    /usr/local/share/boost \
    "${AGENT_TOOLSDIRECTORY:-/opt/hostedtoolcache}/Ruby" \
    "${AGENT_TOOLSDIRECTORY:-/opt/hostedtoolcache}/go" || true

sudo docker image prune --all --force >/dev/null 2>&1 || true

echo "Disk after:"
df -h /
