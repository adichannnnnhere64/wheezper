# Dev shell for building wheezper:
#  - cmake + libclang: whisper.cpp build (whisper-rs) and bindgen
#  - pkg-config + alsa-lib: cpal's ALSA backend
#  - libpulseaudio: PulseAudio host (works over PipeWire's pulse layer)
# Usage: nix-shell
{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  packages = with pkgs; [
    rustc
    cargo
    cmake
    llvmPackages.libclang
    clang
    pkg-config
    alsa-lib
    libpulseaudio
  ];
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
}
