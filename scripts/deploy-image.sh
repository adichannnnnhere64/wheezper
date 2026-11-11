#!/usr/bin/env bash
# Build the wheezper-server image HERE and ship it, instead of building it on
# the server. A Rust release build needs ~3.7 GB of builder cache plus the
# deps, which a small VPS with ~3 GB of headroom cannot afford; the finished
# image is ~30 MB compressed and loads in seconds.
#
#   scripts/deploy-image.sh HOST DEPLOY_DIR [--no-build]
#   scripts/deploy-image.sh hermes /home/ubuntu/migration/wheezper
#
# HOST is an ssh alias/host; DEPLOY_DIR is the folder holding compose.deploy.yml
# (it is started with `docker compose -f compose.deploy.yml up -d --no-build`).
# --no-build ships the image already tagged `wheezper-server` on this machine.
#
# The remote tag must stay `wheezper-server`: compose derives it from
# `name: wheezper` + service `server`. Cross-machine loads are fine as long as
# both are the same architecture (x86_64 here).

set -euo pipefail
cd "$(dirname "$0")/.."

host="${1:?usage: deploy-image.sh HOST DEPLOY_DIR [--no-build]}"
deploy_dir="${2:?usage: deploy-image.sh HOST DEPLOY_DIR [--no-build]}"
build=1
[[ "${3:-}" == "--no-build" ]] && build=0
tag=wheezper-server

if (( build )); then
    echo "wheezper: building $tag from server/Dockerfile"
    docker build -f server/Dockerfile -t "$tag" .
fi
docker image inspect "$tag" >/dev/null 2>&1 || {
    echo "wheezper: image $tag not found (drop --no-build, or docker build -t $tag)" >&2
    exit 1
}

echo "wheezper: shipping $(docker image inspect "$tag" --format '{{.Id}}' | cut -c8-19) to $host:$deploy_dir"
docker save "$tag" | gzip -1 | ssh "$host" 'gunzip | docker load'

# Restart only if the loaded image differs from the running one; compose keeps
# the volume, the certs bind-mount and the network.
ssh "$host" "cd '$deploy_dir' && docker compose -f compose.deploy.yml up -d --no-build server"

echo "wheezper: remote state"
ssh "$host" "cd '$deploy_dir' && docker compose -f compose.deploy.yml ps --format 'table {{.Name}}\t{{.Status}}' && dpkg -l >/dev/null 2>&1; df -h / | tail -1"
