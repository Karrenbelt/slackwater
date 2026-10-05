{ monad ? true }:
let
  rustOverlay = import (builtins.fetchTarball {
    url = "https://github.com/oxalica/rust-overlay/archive/368fee9beaab04ca6fe7af28db63caa9badb22fa.tar.gz";
    sha256 = "153wynqcjizxi46vh9xxf1rw5z7b59jhchma5mnxzv3iyhvwrgg6";
  });

  # Unstable, because Monad execution needs Foundry >= 1.8 and the 25.11 branch ships 1.4.4
  pkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/c59305bab2065cfecc4944690d9eedbb56f3a9fa.tar.gz";
    sha256 = "16rsfnnxk6294sz6asx0shblirkhm4yyvkimq3v00c0y2114gp7b";
  }) {
    overlays = [ rustOverlay ];
  };

  rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

  rustfmtNightly = pkgs.rust-bin.nightly."2026-08-22".minimal.override {
    extensions = [ "rustfmt" ];
  };

  # Foundry leaves its `monad` feature out of the defaults, so the nixpkgs build
  # cannot fork Monad. Enabling it means building Foundry from source; only the
  # fork tests need it, so CI enters with `--arg monad false`.
  foundry =
    if monad then
      pkgs.foundry.overrideAttrs (old: {
        cargoBuildFeatures = (old.cargoBuildFeatures or [ ]) ++ [ "forge/monad" "cast/monad" "chisel/monad" ];
      })
    else
      pkgs.foundry;
in
pkgs.mkShell {
  packages = [
    rustToolchain
    foundry
  ] ++ (with pkgs; [
    jq
    just
    procps
    git
    curl
    cacert
    cargo-machete
  ]);

  # executor/rustfmt.toml sets unstable options, which stable rustfmt drops
  # silently. cargo-fmt reads this variable to pick its rustfmt binary.
  RUSTFMT = "${rustfmtNightly}/bin/rustfmt";

  # A pure shell carries no host CA bundle.
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
}
