# Kani, the bounded model checker for Rust, built from source (it is not in
# nixpkgs).  `cargo kani` then works from the devshell.
#
# Kani's build and driver expect rustup; what they actually use is a
# `$RUSTUP_HOME/toolchains/<name>/lib` directory (for an rpath to
# `librustc_driver`) and a `cargo` that accepts a leading `+<toolchain>`
# argument, so both are provided from the pinned toolchain.  The installed
# layout is Kani's release layout: `bin/`, `lib/`, the precompiled kani
# libraries, and a `toolchain` link, which the driver uses to find cargo.
{
  lib,
  fetchFromGitHub,
  makeRustPlatform,
  makeWrapper,
  runtimeShell,
  runCommand,
  cbmc,
  kissat,
  toolchain,
  toolchainName,
}:
let
  version = "0.68.0";
  rustPlatform = makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };
  # Kani's build compiles Rust's own `std` with the new compiler (a
  # build-std), whose dependencies come from the toolchain's
  # `library/Cargo.lock`, not Kani's.  Vendor both into one directory;
  # Kani's cargo config is kept because it also names the git source.
  kaniVendor = rustPlatform.importCargoLock {
    lockFile = ./kani-Cargo.lock;
    outputHashes = {
      "tracing-tree-0.4.0" = "sha256-YjkXsOn/aLEYxvys9TFZjTyUMPxk3WsI6bbJsbwSiKY=";
    };
  };
  stdVendor = rustPlatform.importCargoLock {
    lockFile = "${toolchain}/lib/rustlib/src/rust/library/Cargo.lock";
  };
  # Kani's build script links `kani-compiler` with an rpath to
  # `$RUSTUP_HOME/toolchains/<name>/lib`; a store path keeps that rpath
  # valid in the output (a build-directory path would be rejected).
  rustupHome = runCommand "kani-rustup-home" { } ''
    mkdir -p "$out/toolchains"
    ln -s "${toolchain}" "$out/toolchains/${toolchainName}"
  '';
  vendor = runCommand "cargo-vendor-dir" { } ''
    mkdir -p "$out"
    cp -r ${kaniVendor}/. "$out/"
    chmod -R u+w "$out"
    cp -rn ${stdVendor}/. "$out/" 2>/dev/null || true
    cp ${kaniVendor}/.cargo/config.toml "$out/.cargo/config.toml"
  '';
in
rustPlatform.buildRustPackage {
  pname = "kani";
  inherit version;

  src = fetchFromGitHub {
    owner = "model-checking";
    repo = "kani";
    tag = "kani-${version}";
    fetchSubmodules = true;
    hash = "sha256-OwjC0I06KxFxnQ4J/599v6ktZKqWgE3Kn/SY8hSk6DY=";
  };

  cargoDeps = vendor;

  nativeBuildInputs = [ makeWrapper ];

  RUSTUP_TOOLCHAIN = toolchainName;
  RUSTC_BOOTSTRAP = "1";

  preBuild = ''
    # The two things Kani wants from rustup.
    mkdir -p "$TMPDIR/shim"
    export RUSTUP_HOME="${rustupHome}"
    cat > "$TMPDIR/shim/cargo" <<'SHIM'
    #!/bin/sh
    case "$1" in +*) shift ;; esac
    exec "$REAL_CARGO" "$@"
    SHIM
    chmod +x "$TMPDIR/shim/cargo"
    export REAL_CARGO="${lib.getExe' toolchain "cargo"}"
    export PATH="$TMPDIR/shim:$PATH"
    export CARGO_NET_OFFLINE=true
  '';

  buildPhase = ''
    runHook preBuild
    cargo build-dev --release
    runHook postBuild
  '';

  # Kani's own release layout (`build-kani bundle`), which is what the
  # driver expects when it is not run from a checkout: binaries, the C and
  # Rust library sources, the precompiled sysroots, the toolchain, and the
  # version records.  `kani` and `cargo-kani` are the driver invoked under
  # those names.
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/bin" "$out/library"
    cp target/release/{kani-driver,kani-compiler} "$out/bin/"
    # `kani-cov` is only built by the `bundle` step; include it if present.
    [ -x target/release/kani-cov ] && cp target/release/kani-cov "$out/bin/"
    cp -r library/kani library/kani_macros library/std "$out/library/"
    cp -r target/kani/{lib,playback,no_core} "$out/"
    ln -s "${toolchain}" "$out/toolchain"
    echo "${toolchainName}" > "$out/rust-toolchain-version"
    rustc --version > "$out/rustc-version"
    cp tools/build-kani/license-notes.txt "$out/"
    wrapProgram "$out/bin/kani-driver" --prefix PATH : "${
      lib.makeBinPath [
        cbmc
        kissat
      ]
    }"
    for n in kani cargo-kani; do
      cat > "$out/bin/$n" <<LAUNCH
    #!${runtimeShell}
    exec -a $n "$out/bin/kani-driver" "\$@"
    LAUNCH
      chmod +x "$out/bin/$n"
    done
    runHook postInstall
  '';

  doCheck = false;

  meta = {
    description = "Bit-precise bounded model checker for Rust";
    homepage = "https://github.com/model-checking/kani";
    license = with lib.licenses; [
      asl20
      mit
    ];
    mainProgram = "kani";
  };
}
