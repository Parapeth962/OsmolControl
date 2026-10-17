#!/bin/sh
# Double-click to start OsmolControl in Terminal. No admin rights needed.
cd "$(dirname "$0")" || exit 1
# Files downloaded by a browser are quarantined; clearing our own files needs no admin.
xattr -d com.apple.quarantine ./bikectl 2>/dev/null
./bikectl start --trainer-bridge
status=$?
echo
echo "OsmolControl stopped (exit $status). You can close this window."
