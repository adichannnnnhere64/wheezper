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
    rustup
    ninja
    # cross-build to Windows (cargo xwin build --target x86_64-pc-windows-msvc)
    llvmPackages.llvm # provides llvm-lib / llvm-ar / llvm-rc
  ];
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
}
