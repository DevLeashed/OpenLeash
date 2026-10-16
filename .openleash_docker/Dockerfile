# Build context for OpenLeash docker workspaces: everything COPYed must live in this folder.
FROM node:22-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends git ripgrep && rm -rf /var/lib/apt/lists/*
