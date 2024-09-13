{ rustDate ? "2024-09-12" }:

let
  mozillaOverlay = import (builtins.fetchTarball "https://github.com/mozilla/nixpkgs-mozilla/archive/9b11a87c0cc54e308fa83aac5b4ee1816d5418a2.tar.gz");
  pkgs = import <nixpkgs> {
    overlays = [ mozillaOverlay ];
  };
  rustChannel = pkgs.rustChannelOf { date = rustDate; channel = "nightly"; };
in pkgs.mkShell {
  nativeBuildInputs = [
    rustChannel.rust
  ];
}
