{
  description = "oc-voice: voice -> text -> opencode";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
          # We intentionally do NOT set config.cudaSupport = true. That would
          # cascade CUDA into every transitive dependency (e.g. onnxruntime
          # would pull nccl built from source, taking hours). We only need
          # CUDA for whisper-rs, which links directly against cudatoolkit/
          # cuda_cudart/libcuda.so below.
        };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rust
            pkg-config
            cmake
            clang
            just
            curl
            whisper-cpp
            llama-cpp
            pipewire
          ];
          buildInputs = with pkgs; [
            # audio
            alsa-lib
            libpulseaudio
            pipewire
            # cuda (whisper.cpp build)
            cudaPackages.cudatoolkit
            cudaPackages.cuda_cudart
            # onnx runtime (silero VAD via voice_activity_detector)
            onnxruntime
            # windowing for the eframe overlay (winit loads these at runtime)
            wayland
            libxkbcommon
            libGL
            libx11
            libxcursor
            libxrandr
            libxi
            # misc
            openssl
            wtype
          ];

          # whisper-rs needs these at build + runtime
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          CUDA_PATH = "${pkgs.cudaPackages.cudatoolkit}";

          # We intentionally avoid CUDA stubs. Link AND run against the real
          # NVIDIA driver exposed by NixOS at /run/opengl-driver/lib. This
          # requires hardware.graphics.enable (opengl.enable on older channels)
          # and the NVIDIA driver to be enabled on the host.
          LIBRARY_PATH =
            "/run/opengl-driver/lib:"
            + pkgs.lib.makeLibraryPath [
              pkgs.cudaPackages.cudatoolkit
              pkgs.cudaPackages.cuda_cudart
            ];

          LD_LIBRARY_PATH =
            "/run/opengl-driver/lib:"
            + pkgs.lib.makeLibraryPath [
              pkgs.cudaPackages.cudatoolkit
              pkgs.cudaPackages.cuda_cudart
              pkgs.alsa-lib
              pkgs.libpulseaudio
              pkgs.pipewire
              pkgs.stdenv.cc.cc.lib
              pkgs.onnxruntime
              # windowing runtime libs for the eframe overlay
              pkgs.wayland
              pkgs.libxkbcommon
              pkgs.libGL
              pkgs.libx11
              pkgs.libxcursor
              pkgs.libxrandr
              pkgs.libxi
            ];

          # voice_activity_detector uses ort with load-dynamic; point it at nixpkgs onnxruntime
          ORT_DYLIB_PATH = "${pkgs.onnxruntime}/lib/libonnxruntime.so";

          shellHook = ''
            echo "oc-voice dev shell ready"
            echo "  just fetch-model   # download ggml-base.en.bin"
            echo "  just run           # run the POC"
          '';
        };
      });
}
