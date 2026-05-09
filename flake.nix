{
  description = "tweaker";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    naersk.url = "github:nix-community/naersk";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      naersk,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = (
          pkgs.rust-bin.nightly.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
          }
        );
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust
            pkg-config
            openssl
            nixfmt
          ];
        };
      }
    );
}
