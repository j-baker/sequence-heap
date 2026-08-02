# The crate, built by crate2nix.  `Cargo.nix` is generated from `Cargo.lock`
# at evaluation time (import-from-derivation), so there is nothing to
# regenerate or commit; the source is filtered to what the build reads, so
# documentation and Nix edits do not re-trigger it.
#
# Returns `package` (the library) and `tests` (the same build with the
# crate's tests compiled and run, which is what `nix flake check` uses).
{ pkgs, crate2nixTools }:
let
  inherit (pkgs) lib;
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../tests
      ../examples
      ../benches
      ../README.md
      ../LICENSE-MIT
      ../LICENSE-APACHE
    ];
  };
  cargoNix = crate2nixTools.appliedCargoNix {
    name = "sequence-heap";
    inherit src;
  };
  crateOverrides = pkgs.defaultCrateOverrides // {
    sequence-heap = attrs: {
      meta = (attrs.meta or { }) // {
        description = "A cache-efficient priority queue with the std BinaryHeap API";
        homepage = "https://github.com/j-baker/sequence-heap";
        license = with lib.licenses; [
          mit
          asl20
        ];
      };
    };
  };
  build = runTests: cargoNix.rootCrate.build.override { inherit crateOverrides runTests; };
in
{
  package = build false;
  tests = build true;
}
