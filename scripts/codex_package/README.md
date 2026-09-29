# Codex package builder

This package contains the implementation behind `scripts/build_codex_package.py`.
The top-level script is the stable executable entry point; these modules keep the
package-building logic split by responsibility.

Run the builder through `just`:

```bash
just assemble-codex-package --help
just assemble-codex-package --variant codex-app-server
just assemble-codex-package --target x86_64-unknown-linux-gnu
```

The builder creates a canonical Codex package directory:

```text
.
├── codex-package.json
├── bin
│   ├── <entrypoint>[.exe]
│   └── codex-code-mode-host[.exe]
├── codex-resources
│   ├── bwrap                             # Linux only
│   ├── zsh/bin/zsh                       # supported Unix targets only
│   ├── codex-command-runner.exe          # Windows only
│   └── codex-windows-sandbox-setup.exe   # Windows only
└── codex-path
    └── rg[.exe]
```

The package directory is the primary artifact. Archive formats such as
`.tar.gz`, `.tar.zst`, and `.zip` are serializations of that directory.

For an opt-in CompleteDotTech package directory candidate, pass
`--fork-base-commit` with a full caller-declared ancestor commit. The builder
requires a clean source tree and source-built inputs, then records the current
fork commit, SHA-256 of the base-to-fork Git diff, preview/stable channel label,
and the currently qualified SQLite-only storage capability in
`codex-fork-package.json`. This does not authenticate the base as OpenAI upstream.
Run `python scripts/verify_fork_package.py <package-dir>` to check the staged
directory. The manifest inventories all files and directories, their byte hashes,
and Unix mode claims. A Windows check of a Unix package reports
`unixModeStatus=unavailable`; run that check on Unix before relying on modes.
`python scripts/verify_fork_archive.py <package-dir> <archive>` checks an existing
ZIP or TAR archive against the sealed candidate directory, including serialized
entries, byte hashes, types, and Unix modes. It is read-only and returns the
archive SHA-256 on success. Fork candidate archive outputs and prebuilt binary
overrides remain refused by the builder. A future release channel must
authenticate the manifest before downloads or installation can trust it. This
option does not activate the fork installer, updater, or PostgreSQL storage.

`fork_archive_publication.py` contains an inactive Linux helper for publishing
one already verified archive into a caller-held, owner-private directory
descriptor. It binds the final entry to that directory's inode and the verified
bytes, and uses `renameat2(RENAME_NOREPLACE)` to avoid replacing a competing
entry. The destination must be on a different filesystem from the sealed
package, preventing a directory rename into that package during publication.
The returned receipt must be rechecked through the same descriptor
before later use; it makes no claim that a pathname remains stable. Failed
attempts may leave a staged or final file for explicit reconciliation. There is
no multi-archive or checksum transaction. Other platforms and release-channel
authentication remain unimplemented; the builder continues to refuse fork
archive output everywhere.

If `--target` is omitted, the builder uses the release target for the current
host platform. On Linux, that default is a musl target to match Codex release
artifacts; pass a GNU Linux target explicitly for native glibc local builds. If
`--package-dir` is omitted, the builder creates a new temporary directory and
prints its path after the package is built.

The `--variant` flag selects the package entrypoint. Supported variants are
`codex` and `codex-app-server`. The `--package-version` flag sets the version in
`codex-package.json`; it defaults to `[workspace.package].version` in
`codex-rs/Cargo.toml`.

## Source-built artifacts

Artifacts built from this repository are built by the package builder in one
grouped `cargo build` command per package when they are needed and no prebuilt
override was provided:

- all targets: the selected entrypoint, unless `--entrypoint-bin` is provided
- all targets: `codex-code-mode-host`, unless `--code-mode-host-bin` is provided
- Linux targets: `bwrap`, unless `--bwrap-bin` is provided
- Windows targets: `codex-command-runner` and `codex-windows-sandbox-setup`,
  unless the corresponding prebuilt helper flags are provided

The default cargo profile is `dev-small` because local iteration should favor
fast, small builds. Release jobs should pass `--cargo-profile release` and an
explicit target. Release jobs that already built and signed/notarized the
entrypoint should pass `--entrypoint-bin` so the package contains that exact
binary instead of rebuilding it.

Release jobs should likewise pass `--code-mode-host-bin` so the package contains
the signed host executable beside the signed entrypoint.

Release jobs that already built package resource binaries should also pass the
corresponding resource flags: `--bwrap-bin` for Linux packages, and
`--codex-command-runner-bin` plus `--codex-windows-sandbox-setup-bin` for
Windows packages. This keeps package archive creation as a pure staging step
after signing instead of rebuilding resources.

When the builder source-builds an entrypoint for a Darwin or Linux target, it
downloads and verifies the matching Codex-built V8 release pair before invoking
Cargo and sets `RUSTY_V8_ARCHIVE` plus `RUSTY_V8_SRC_BINDING_PATH` for that
build. Windows targets keep Cargo's release-build MSVC artifact path. Explicit
overrides remain authoritative when both variables are already set. Set
`V8_FROM_SOURCE=1` to leave the build with the `v8` crate source-build path.

`rg` is not built from this repository, so the builder fetches it from the
DotSlash manifest at `scripts/codex_package/rg`. Downloaded archives are cached
under `$TMPDIR/codex-package/<target>-rg` and are reused only after the recorded
size and SHA-256 digest have been verified. Pass `--rg-bin` to use a local
ripgrep executable instead.

The patched zsh fork used by `shell_zsh_fork` is fetched from the DotSlash
manifest at `scripts/codex_package/codex-zsh` when the selected target has a
matching prebuilt artifact. Downloaded archives are cached under
`$TMPDIR/codex-package/<target>-zsh` and installed at
`codex-resources/zsh/bin/zsh`. Pass `--zsh-bin` to package a prebuilt, signed
executable, or `--zsh-manifest` to use a different DotSlash manifest, such as
the manifest published with a standalone zsh artifact release.
