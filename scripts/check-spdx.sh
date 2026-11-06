#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Fails if any source-like file under version control lacks an SPDX header.
set -euo pipefail
cd "$(dirname "$0")/.."

missing=0
while IFS= read -r f; do
  case "$f" in
    *.rs|*.toml|*.sh|*.yml|*.yaml|*.html|*.css|*.ts|*.js|*.service|*.conf|*.policy|*.desktop|*.md)
      if ! head -5 "$f" | grep -q "SPDX-License-Identifier: Apache-2.0"; then
        echo "missing SPDX header: $f"
        missing=1
      fi
      ;;
  esac
done < <(git ls-files | grep -v -E '^(LICENSE|CLAUDE\.md|README\.md|Cargo\.lock)$' | grep -v '\.gitkeep$')

if [ "$missing" -ne 0 ]; then
  echo "Add '// SPDX-License-Identifier: Apache-2.0' (or the comment style for that file type) near the top."
  exit 1
fi
echo "SPDX headers OK"
