{
  description = "yomu - manga/webtoon library, reader and downloader";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
  }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs {
      inherit system;
      overlays = [rust-overlay.overlays.default];
      # Android SDK/NDK for the mobile shell
      config.allowUnfree = true;
      config.android_sdk.accept_license = true;
    };
    inherit (pkgs) lib;

    androidNdkVersion = "27.0.12077973";
    androidComposition = pkgs.androidenv.composeAndroidPackages {
      # what the tauri-generated gradle project compiles against
      platformVersions = ["34" "36"];
      buildToolsVersions = ["34.0.0" "35.0.0"];
      includeNDK = true;
      ndkVersion = androidNdkVersion;
    };

    version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

    rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
    rustPlatform = pkgs.makeRustPlatform {
      cargo = rustToolchain;
      rustc = rustToolchain;
    };

    # Same scheme as chaos: wasm-bindgen-cli pinned by Cargo.lock so the CLI
    # and crate versions cannot drift. Refresh both hashes when the locked
    # wasm-bindgen version changes (nix prints the expected hash).
    hasCargoLock = builtins.pathExists ./Cargo.lock;

    wasm-bindgen-cli = let
      cargoLock = builtins.fromTOML (builtins.readFile ./Cargo.lock);
      wasmBindgen =
        lib.findFirst
        (p: p.name == "wasm-bindgen")
        (throw "wasm-bindgen not found in Cargo.lock")
        cargoLock.package;
    in
      pkgs.buildWasmBindgenCli rec {
        src = pkgs.fetchCrate {
          pname = "wasm-bindgen-cli";
          version = wasmBindgen.version;
          hash = "sha256-H6Is3fiZVxZCfOMWK5dWMSrtn50VGv0sfdnsT+cTtyk=";
        };

        cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
          inherit src;
          inherit (src) pname version;
          hash = "sha256-VucqkXbCi4qtQzY/HrXiDnbSURsagPsdNVMn1Tw3UiY=";
        };
      };

    yomu-server = rustPlatform.buildRustPackage {
      pname = "yomu-server";
      inherit version;
      src = self;

      cargoLock.lockFile = ./Cargo.lock;
      cargoBuildFlags = ["-p" "yomu-server"];
      cargoTestFlags = ["-p" "yomu-server" "-p" "yomu-source"];

      meta = {
        description = "yomu backend: manga library, downloader, progress tracking";
        mainProgram = "yomu-server";
      };
    };

    yomu-web = pkgs.stdenv.mkDerivation {
      pname = "yomu-web";
      inherit version;
      src = self;

      cargoDeps = pkgs.rustPlatform.importCargoLock {lockFile = ./Cargo.lock;};

      nativeBuildInputs = [
        rustToolchain
        pkgs.trunk
        pkgs.binaryen
        wasm-bindgen-cli
        pkgs.rustPlatform.cargoSetupHook
      ];

      buildPhase = ''
        runHook preBuild
        export HOME=$TMPDIR
        cd crates/yomu-web
        trunk build --release --offline true --dist dist
        runHook postBuild
      '';

      installPhase = ''
        runHook preInstall
        cp -r dist $out
        runHook postInstall
      '';

      meta.description = "yomu web frontend (static trunk dist)";
    };
  in {
    packages.${system} = {
      inherit yomu-server yomu-web;
      default = yomu-server;
    };

    nixosModules = {
      yomu = import ./nix/module.nix self;
      default = self.nixosModules.yomu;
    };

    devShells.${system} = {
      default = pkgs.mkShell {
        name = "yomu";

        packages = with pkgs;
          [
            rustToolchain
            trunk
            binaryen
            just
          ]
          ++ lib.optional hasCargoLock wasm-bindgen-cli;
      };

      # Desktop/mobile shell development: `nix develop .#tauri`. Adds the
      # Linux webview stack and the tauri CLI on top of the default shell.
      tauri = pkgs.mkShell {
        name = "yomu-tauri";

        packages = with pkgs;
          [
            rustToolchain
            trunk
            binaryen
            just
            cargo-tauri
            pkg-config
          ]
          ++ lib.optional hasCargoLock wasm-bindgen-cli;

        buildInputs = with pkgs; [
          gtk3
          webkitgtk_4_1
          libsoup_3
          openssl
          glib
          cairo
          pango
          gdk-pixbuf
          atk
          librsvg
        ];
      };

      # Android build of the shell: `nix develop .#android`, then
      # `cargo tauri android build --apk --target aarch64`.
      android = pkgs.mkShell {
        name = "yomu-android";

        packages = with pkgs;
          [
            rustToolchain
            trunk
            binaryen
            just
            cargo-tauri
            jdk17
            androidComposition.androidsdk
          ]
          ++ lib.optional hasCargoLock wasm-bindgen-cli;

        env = rec {
          JAVA_HOME = pkgs.jdk17.home;
          ANDROID_HOME = "${androidComposition.androidsdk}/libexec/android-sdk";
          NDK_HOME = "${ANDROID_HOME}/ndk/${androidNdkVersion}";
        };

        # The tauri CLI insists on `rustup target add`; the rust-overlay
        # toolchain already ships every Android target, so a no-op is honest.
        shellHook = ''
          shim_dir=$(mktemp -d)
          printf '#!/bin/sh\nexit 0\n' > "$shim_dir/rustup"
          chmod +x "$shim_dir/rustup"
          export PATH="$shim_dir:$PATH"
        '';
      };
    };

    formatter.${system} = pkgs.alejandra;
  };
}
