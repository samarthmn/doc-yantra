#!/usr/bin/env bash
# Moved: the release covers every published package (pdf-core AND office-core),
# so the script lives at the repo root as scripts/release.sh. This forwarder
# keeps the old path working; feel free to delete it once habits catch up.
echo "note: pdf-core/scripts/release.sh moved to scripts/release.sh — forwarding." >&2
exec "$(dirname "$0")/../../scripts/release.sh" "$@"
