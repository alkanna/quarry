#!/bin/sh
# With a Developer ID, dev builds sign as the release app (dev.quarry.app) so both share the keychain "Always Allow".
# Else the self-signed "Quarry Dev" cert, else unsigned. No timestamp, so signing works offline.
id=$(security find-identity -v -p codesigning 2>/dev/null | awk '/Developer ID Application/ {print $2; exit}')
if [ -n "$id" ]; then ident=dev.quarry.app; else id="Quarry Dev"; ident=dev.quarry; fi
codesign -f -s "$id" --identifier "$ident" --timestamp=none "$1" 2>/dev/null || echo "run-signed: no signing certificate found, running unsigned (setup in README)" >&2
exec "$@"
