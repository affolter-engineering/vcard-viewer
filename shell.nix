# Legacy nix-shell entry point.
# Usage:  nix-shell
# For the flake-based workflow use: nix develop
{ pkgs ? import <nixpkgs> {} }:

let
  # Use rust-overlay when available; fall back to nixpkgs Rust otherwise.
  rustOverlay =
    let src = builtins.fetchTarball "https://github.com/oxalica/rust-overlay/archive/master.tar.gz";
    in import src;

  pkgsWithRust = import <nixpkgs> {
    overlays = [ rustOverlay ];
    inherit (pkgs) system;
  };

  rustToolchain = pkgsWithRust.rust-bin.stable.latest.default.override {
    extensions = [ "rust-src" "clippy" "rustfmt" ];
  };
in
pkgsWithRust.mkShell {
  buildInputs = [
    rustToolchain
    pkgsWithRust.pkg-config
    pkgsWithRust.rust-analyzer
  ];

  RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
}
