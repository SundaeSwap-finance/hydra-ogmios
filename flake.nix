{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
    nixpkgs-mozilla = {
      url = "github:mozilla/nixpkgs-mozilla";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, utils, naersk, nixpkgs-mozilla }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = (import nixpkgs) {
          inherit system;
          overlays = [
            (import nixpkgs-mozilla)
          ];
        };
        toolchain = (pkgs.rustChannelOf {
          rustToolchain = ./rust-toolchain;
          sha256 = "";
        }).rust;
        naersk-lib = pkgs.callPackage naersk {
          cargo = toolchain;
          rustc = toolchain;
        };
      in rec {
        defaultPackage = naersk-lib.buildPackage {
          src = ./.;
        };
        devShell = pkgs.mkShell {
          nativeBuildInputs = [ toolchain ];
        };
      }
    );
}
