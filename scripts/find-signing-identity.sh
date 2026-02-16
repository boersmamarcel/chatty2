#!/bin/bash
# Helper script to find your code signing identity

echo "=== Available Code Signing Identities ==="
echo ""

security find-identity -v -p codesigning

echo ""
echo "=== Instructions ==="
echo "1. Copy the full name in quotes (e.g., \"Developer ID Application: Your Name (TEAM_ID)\")"
echo "2. Set it as an environment variable:"
echo "   export SIGNING_IDENTITY=\"Developer ID Application: Your Name (TEAM_ID)\""
echo ""
echo "3. Then run: ./scripts/package-macos.sh"
echo ""
echo "For GitHub Actions, add this as the MACOS_SIGNING_IDENTITY secret"
