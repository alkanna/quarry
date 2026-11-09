#!/bin/sh
# Falls back to an unsigned run when the "Quarry Dev" certificate is missing.
codesign -f -s "Quarry Dev" --identifier dev.quarry "$1" 2>/dev/null || echo "run-signed: 'Quarry Dev' cert not found, running unsigned (setup in README)" >&2
exec "$@"
