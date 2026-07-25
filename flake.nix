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
    # The flake source has no .git for the build scripts to ask; hand the
    # commit over explicitly (shown on the About page / health endpoint).
    buildCommit = self.shortRev or self.dirtyShortRev or "unknown";

    rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

    # Panic locations otherwise embed ${rustToolchain}/lib/rustlib/src/...,
    # which Nix reads as a runtime reference and follows into rustc, docs,
    # rust-analyzer, clippy, rustfmt and gcc — 2.4 GiB of build tooling in the
    # closure of a 13 MB binary. The remapped path was never resolvable on a
    # user's machine anyway.
    remapPathPrefix = "--remap-path-prefix=${rustToolchain}=/rust-toolchain";

    # The native builds must additionally re-state a flag that is not ours:
    # cargoSetupHook writes -Cforce-frame-pointers=yes into .cargo/config.toml
    # under [target.<host>], and cargo takes rustflags from the first source
    # that applies rather than merging them, so a RUSTFLAGS environment
    # variable silently drops it. Profilers and backtraces rely on it.
    #
    # This is a hand-transcribed copy of what that hook currently writes. If a
    # nixpkgs bump adds another flag to that table, this copy silently drops
    # it — re-read `cargoSetupHook`'s generated .cargo/config.toml whenever the
    # nixpkgs input moves.
    remapRustflags = "-Cforce-frame-pointers=yes ${remapPathPrefix}";

    rustPlatform = pkgs.makeRustPlatform {
      cargo = rustToolchain;
      rustc = rustToolchain;
    };

    # Every native rust package goes through here rather than setting
    # env.RUSTFLAGS by hand: an opt-in flag repeated per package is one a new
    # package forgets, and forgetting it costs a 40× closure with no error.
    # RUSTFLAGS is set last on purpose — it is a property of how we build, not
    # something a call site gets to override.
    buildYomuRustPackage = args:
      rustPlatform.buildRustPackage (args
        // {
          env = (args.env or {}) // {RUSTFLAGS = remapRustflags;};
        });

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

    tauriLibs = with pkgs; [
      webkitgtk_4_1
      gtk3
      libsoup_3
      glib
      cairo
      pango
      gdk-pixbuf
      atk
      librsvg
      openssl
      dbus
    ];

    yomu-server = buildYomuRustPackage {
      pname = "yomu-server";
      inherit version;
      src = self;

      cargoLock.lockFile = ./Cargo.lock;
      cargoBuildFlags = ["-p" "yomu-server"];
      cargoTestFlags = ["-p" "yomu-server" "-p" "yomu-source"];
      env.YOMU_BUILD_COMMIT = buildCommit;

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
      YOMU_BUILD_COMMIT = buildCommit;

      # The same remap as the native packages, so panic strings in the wasm
      # stop naming a store path — but without their -Cforce-frame-pointers,
      # which there is nothing here to preserve: cargoSetupHook writes it under
      # [target."x86_64-unknown-linux-gnu"], a table that never applies to the
      # wasm32 units trunk builds, and with --target set cargo passes no
      # rustflags to host build-script units either.
      RUSTFLAGS = remapPathPrefix;

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

    # What the server serves: the trunk dist plus brotli/gzip siblings built
    # once here, so ServeDir never compresses per request. Kept separate from
    # yomu-web because yomu-desktop bakes that one into the binary, where the
    # siblings would be ~1.08 MB (1 080 828 B measured) of files the asset
    # protocol never serves.
    yomu-web-compressed =
      pkgs.runCommand "yomu-web-compressed-${version}" {
        nativeBuildInputs = [pkgs.brotli pkgs.gzip];
        meta.description = "yomu web frontend with precompressed siblings";
      } ''
        cp -r ${yomu-web} $out
        chmod -R u+w $out
        # The file list is collected before anything is written: the loop
        # creates .br/.gz entries in the very directory find is reading, and
        # whether a new entry shows up in an in-progress readdir is
        # unspecified — so a large enough dist could compress its own output.
        find $out -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.css' \
          -o -name '*.html' -o -name '*.json' -o -name '*.svg' \
          -o -name '*.webmanifest' \) -print0 > "$TMPDIR/targets"
        while IFS= read -r -d "" f; do
          brotli -q 11 -f -o "$f.br" "$f"
          gzip -9 -c "$f" > "$f.gz"
        done < "$TMPDIR/targets"
      '';

    # Desktop shell (same scheme as chaos-desktop): generate_context! bakes
    # the web dist into the binary at compile time, so the yomu-web output
    # is copied in place before cargo runs. wrapGAppsHook3 wires GSettings
    # schemas + TLS (glib-networking), without which WebKitGTK apps crash or
    # fail https at runtime.
    yomu-desktop = buildYomuRustPackage {
      pname = "yomu-desktop";
      inherit version;
      src = self;

      cargoLock.lockFile = ./Cargo.lock;

      cargoBuildFlags = ["-p" "yomu-shell"];
      cargoTestFlags = ["-p" "yomu-shell"];
      env.YOMU_BUILD_COMMIT = buildCommit;

      nativeBuildInputs = with pkgs; [pkg-config wrapGAppsHook3];
      buildInputs = tauriLibs ++ [pkgs.glib-networking];

      preBuild = ''
        rm -rf crates/yomu-web/dist
        cp -r ${yomu-web} crates/yomu-web/dist
      '';

      postInstall = ''
        # yomu-shell is a staticlib/cdylib/rlib because the Android build
        # needs the first two; buildRustPackage installs whatever it finds,
        # and the .a alone is five sixths of this output. Only bin/yomu-shell
        # ever runs on the desktop.
        rm -f $out/lib/libyomu_shell_lib.a

        install -Dm644 crates/yomu-shell/icons/128x128.png \
          $out/share/icons/hicolor/128x128/apps/yomu.png
        install -Dm644 crates/yomu-shell/icons/32x32.png \
          $out/share/icons/hicolor/32x32/apps/yomu.png
        mkdir -p $out/share/applications
        cat > $out/share/applications/yomu.desktop <<INI
        [Desktop Entry]
        Name=yomu
        Comment=Manga and webtoon library and reader
        Exec=yomu-shell
        Icon=yomu
        Type=Application
        Categories=Utility;
        INI
      '';

      meta = {
        description = "yomu desktop shell (Tauri)";
        mainProgram = "yomu-shell";
      };
    };
  in {
    packages.${system} = {
      inherit yomu-server yomu-web yomu-web-compressed yomu-desktop;
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
            cargo-nextest
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

        buildInputs = tauriLibs;
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
