#!/usr/bin/env bash
# Map upstream zeronsh/zeron code onto Mast's internal names, so an upstream
# merge or cherry-pick conflicts only where the code really diverged.
#
#   scripts/rename-map.sh            rewrite the working tree (and git mv paths)
#   scripts/rename-map.sh < a.patch  rewrite a patch on stdout (git format-patch)
#
# Covers the crate/module/path renames (docs/plans/rename-migration.md §7).
# Runtime names that kept an alias (env vars, artifact scheme, ...) are NOT
# mapped: upstream spells them the old way and Mast still reads that spelling.
set -euo pipefail

CRATES='proto|rpc|engine|harness|doc|sync|ui|syntax|theme|update|mcp|preview'
SUBST="s/\\bzeron([-_])($CRATES)\\b/mast\$1\$2/g; \
s#apps/zeron\\b#apps/mast#g; \
s#crates/mcp/src/zeron\\.rs#crates/mcp/src/mast.rs#g; \
s#\\bcrate::zeron::#crate::mast::#g; \
s#\\bmod zeron;#mod mast;#g; \
s#\\bpub use zeron::\\{Zeron,#pub use mast::{Mast,#g; \
s#((?:-p|--package) )zeron(?![-_\\w])#\$1mast#g"

if [ ! -t 0 ]; then
  perl -pe "$SUBST"
  exit 0
fi

cd "$(git rev-parse --show-toplevel)"
[ -d apps/zeron ] && git mv apps/zeron apps/mast
[ -f crates/mcp/src/zeron.rs ] && git mv crates/mcp/src/zeron.rs crates/mcp/src/mast.rs
[ -f crates/theme/src/bin/zeron-theme-import.rs ] &&
  git mv crates/theme/src/bin/zeron-theme-import.rs crates/theme/src/bin/mast-theme-import.rs
git ls-files -z -- '*.rs' '*.toml' '*.sh' '*.yml' '*.md' |
  xargs -0 perl -pi -e "$SUBST"
# The mcp wrapper type `Zeron` is only a type in the mcp crate.
git ls-files -z -- 'crates/mcp/*.rs' | xargs -0 perl -pi -e 's/\bZeron\b/Mast/g'
