#!/usr/bin/env bash
# Sync MDX docs from layers/*/src/ to docs/src/content/docs/layers/
# Source of truth: .mdx files colocated with .rs files in layers/
# Run before `npx astro build`.
set -euo pipefail

DOCS_DIR="docs/src/content/docs/layers"

# Clean old synced docs
rm -rf "$DOCS_DIR"

# Find and copy all .mdx files from layers
for mdx in $(find layers -path '*/src/*.mdx' -not -path '*/tests/*' | sort); do
    # layers/core/src/crypto.mdx → layers/core/crypto.mdx → docs path
    rel=$(echo "$mdx" | sed 's|layers/||' | sed 's|/src/|/|')
    target="$DOCS_DIR/$rel"

    mkdir -p "$(dirname "$target")"
    cp "$mdx" "$target"
done

count=$(find "$DOCS_DIR" -name '*.mdx' | wc -l)
echo "Synced $count docs from layers to $DOCS_DIR"
