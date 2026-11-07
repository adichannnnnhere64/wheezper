# Dev shell for building wheezper (whisper-rs needs cmake + libclang for bindgen).
# Usage: nix-shell
{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  packages = with pkgs; [
    rustc
    cargo
    cmake
    llvmPackages.libclang
    clang
  ];
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
}
