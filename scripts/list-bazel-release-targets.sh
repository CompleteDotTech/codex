#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

# Compile production Rust crates with release assertions disabled. Test
# executables are covered by the test matrix; building every test binary here
# exhausts the hosted Linux runner before this release check can finish.
# Exclude the experimental V8 proof of concept from this sweep.
bazelisk query --output=label \
  'attr("testonly", "^0$", kind("rust_(binary|library|proc_macro) rule", //codex-rs/... except //codex-rs/v8-poc/...))'
