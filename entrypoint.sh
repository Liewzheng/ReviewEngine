#!/bin/bash
set -e
mkdir -p /app/frontend/dist
if [ -n "$REVIEW_API_TOKEN" ]; then
  printf '{"apiToken":"%s"}\n' "$REVIEW_API_TOKEN" > /app/frontend/dist/config.json
fi
exec /usr/local/bin/review-engine "$@"
