{
  description = "sequence-heap: a cache-efficient priority queue with the BinaryHeap API";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
    crate2nix = {
      url = "github:nix-community/crate2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Nightly toolchains, for Kani only.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix-claude-code = {
      url = "github:ryoppippi/nix-claude-code";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crate2nix,
      rust-overlay,
      nix-claude-code,
    }:
    let
      inherit (nixpkgs) lib;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          config.allowUnfree = true;
          overlays = [
            rust-overlay.overlays.default
            nix-claude-code.overlays.default
          ];
        };

      # The crate, built by crate2nix with Cargo.nix generated at evaluation
      # time from Cargo.lock (import-from-derivation): see nix/crate.nix.
      crate =
        pkgs:
        import ./nix/crate.nix {
          inherit pkgs;
          crate2nixTools = crate2nix.tools.${pkgs.stdenv.hostPlatform.system};
        };

      # Kani is pinned to a nightly; the same toolchain serves the kani shell.
      kaniToolchainName = "nightly-2026-08-21";
      kaniToolchain =
        pkgs:
        pkgs.rust-bin.nightly."2026-08-21".default.override {
          extensions = [
            "llvm-tools"
            "rustc-dev"
            "rust-src"
            "rustfmt"
          ];
        };
      kani =
        pkgs:
        pkgs.callPackage ./nix/kani.nix {
          toolchain = kaniToolchain pkgs;
          toolchainName = kaniToolchainName;
        };
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = pkgsFor system;
      in
      {
        packages = {
          sequence-heap = (crate pkgs).package;
          default = (crate pkgs).package;
          # `nix build .#kani`: the model checker, for `cargo kani` in the shell.
          kani = kani pkgs;
          # `nix run .#release -- <version> [--dry-run]`: see nix/release.nix.
          release = import ./nix/release.nix { inherit pkgs; };
        };

        apps.release = {
          type = "app";
          program = lib.getExe self.packages.${system}.release;
        };

        # `nix flake check` is the CI gate: the crate builds and its tests pass.
        checks = {
          build = (crate pkgs).package;
          tests = (crate pkgs).tests;
        };

        devShells = {
          default = pkgs.mkShell {
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.rustfmt
              pkgs.clippy
              pkgs.rust-analyzer
              pkgs.claude-code
            ]
            # Profiling: cachegrind/callgrind and hardware counters.
            ++ lib.optionals pkgs.stdenv.isLinux [
              pkgs.valgrind
              pkgs.linuxPackages.perf
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };

          # `nix develop .#kani`: the pinned nightly plus Kani, for
          # `cargo kani --harness <name>` on the proof harnesses.
          kani = pkgs.mkShell {
            packages = [
              (kaniToolchain pkgs)
              (kani pkgs)
            ];
            RUST_SRC_PATH = "${kaniToolchain pkgs}/lib/rustlib/src/rust/library";
          };
        };

        # `nix fmt` / `nix fmt -- --ci`.
        formatter = pkgs.nixfmt-tree;
      }
    )
    // {
      overlays.default = final: _prev: {
        sequence-heap = (crate final).package;
      };
    };
}
