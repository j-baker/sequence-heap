# `nix run .#release -- <version> [--dry-run] [--allow-dirty] [--publish]`
#
# Prepares a release: bumps the version in Cargo.toml and Cargo.lock,
# verifies that the Nix build and tests still pass,
# runs `cargo publish --dry-run`, then commits `Release v<version>` and tags
# `v<version>`.  Pushing the tag (`git push --follow-tags`) makes CI publish
# to crates.io through Trusted Publishing (.github/workflows/publish.yaml),
# so no token is needed locally.
#
# `--publish` also runs `cargo publish` here, with a token from
# `CARGO_REGISTRY_TOKEN` or a prior `cargo login`; crates.io requires the
# very first release of a crate to be published this way before Trusted
# Publishing can be configured.  `--dry-run` stops before committing and
# restores the two files.
#
# This cannot be a Nix build (the sandbox has no network and no git
# identity), so it is an app with pinned tools.
{ pkgs }:
pkgs.writeShellApplication {
  name = "release";
  runtimeInputs = [
    pkgs.cargo
    pkgs.rustc
    pkgs.cargo-edit
    pkgs.git
    pkgs.nix
  ];
  text = ''
    usage() {
      echo "usage: release <version> [--dry-run] [--allow-dirty] [--publish]" >&2
      exit 2
    }
    version=""
    dry_run=0
    allow_dirty=0
    publish=0
    for arg in "$@"; do
      case "$arg" in
        --dry-run) dry_run=1 ;;
        --allow-dirty) allow_dirty=1 ;;
        --publish) publish=1 ;;
        -*) usage ;;
        *)
          if [[ -z "$version" ]]; then version="$arg"; else usage; fi
          ;;
      esac
    done
    [[ -n "$version" ]] || usage
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.]+)?$ ]] || {
      echo "not a semver version: $version" >&2
      exit 2
    }

    cd "$(git rev-parse --show-toplevel)" || exit 1
    if [[ $allow_dirty -eq 0 && -n "$(git status --porcelain)" ]]; then
      echo "working tree is not clean; commit first (or pass --allow-dirty for a dry run)" >&2
      exit 1
    fi
    if [[ $dry_run -eq 0 ]] && git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
      echo "tag v$version already exists" >&2
      exit 1
    fi

    restore() { git checkout -- Cargo.toml Cargo.lock; }
    [[ $dry_run -eq 1 ]] && trap restore EXIT

    echo "==> setting version $version"
    cargo set-version "$version"
    echo "==> verifying the Nix build and tests"
    nix build --no-link "path:.#checks.${pkgs.stdenv.hostPlatform.system}.build" "path:.#checks.${pkgs.stdenv.hostPlatform.system}.tests"
    echo "==> cargo publish --dry-run"
    cargo publish --dry-run --allow-dirty --locked

    if [[ $dry_run -eq 1 ]]; then
      echo "==> dry run complete; version files restored"
      exit 0
    fi

    echo "==> committing and tagging v$version"
    git add Cargo.toml Cargo.lock
    git commit -m "Release v$version"
    git tag -a "v$version" -m "sequence-heap v$version"

    if [[ $publish -eq 1 ]]; then
      echo "==> cargo publish"
      cargo publish --locked
      echo "==> published v$version; push with: git push --follow-tags"
    else
      echo "==> tagged v$version; push with: git push --follow-tags"
      echo "    CI will publish to crates.io through Trusted Publishing."
    fi
  '';
}
