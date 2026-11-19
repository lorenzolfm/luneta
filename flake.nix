{
  description = "luneta — zellij beside a sidebar of its sessions and agents, and its daemon";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
    ghostty = {
      url = "github:ghostty-org/ghostty/108cacb17ed8990840cd14abfce43d8ed8503ca1";
      flake = false;
    };
    nixpkgs-zig.url = "github:nixos/nixpkgs/d4f25a3e562008648d45bcf2d70bcc7422cad84e";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      ghostty,
      nixpkgs-zig,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      inherit (cargoToml.workspace.package) version;

      build =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          craneLib = ((crane.mkLib pkgs).overrideToolchain toolchain).overrideScope (
            _: prev: { writeTOML = prev.writeTOMLViaRemarshal; }
          );

          cargoFiles = pkgs.lib.fileset.unions [
            ./.cargo/config.toml
            ./crates
            ./proto
            ./skills/luneta/SKILL.md
            ./Cargo.toml
            ./Cargo.lock
          ];

          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = cargoFiles;
          };

          lintSrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              cargoFiles
              ./clippy.toml
            ];
          };

          testSrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              cargoFiles
              ./.config/nextest.toml
              ./shell/luneta.fish
            ];
          };

          denySrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              cargoFiles
              ./deny.toml
            ];
          };

          speechBuild = [
            pkgs.cmake
            pkgs.pkg-config
          ];
          speechLibs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.alsa-lib ];

          ghosttyRevs = {
            "git+https://github.com/uzaaft/libghostty-rs?rev=cd4d497e8c698b27054a677ce19d1bd1a1a4d8bf#cd4d497e8c698b27054a677ce19d1bd1a1a4d8bf" =
              "108cacb17ed8990840cd14abfce43d8ed8503ca1";
          };
          ghosttySys =
            pkgs.lib.findFirst (crate: crate.name == "libghostty-vt-sys")
              (throw "Cargo.lock has no libghostty-vt-sys")
              (builtins.fromTOML (builtins.readFile ./Cargo.lock)).package;
          ghosttySysRev =
            ghosttyRevs.${ghosttySys.source}
              or (throw "ghosttyRevs has no commit for libghostty-vt-sys from ${ghosttySys.source}");
          libghosttyVt =
            pkgs.lib.throwIfNot (ghostty.rev == ghosttySysRev)
              "the ghostty input is not ${ghosttySysRev}, which libghostty-vt-sys binds"
              (
                (pkgs.lib.makeScope pkgs.newScope (_: {
                  inherit (nixpkgs-zig.legacyPackages.${system}) zig_0_17;
                })).callPackage
                  "${ghostty}/nix/libghostty-vt.nix"
                  {
                    optimize = "ReleaseSafe";
                    revision = ghostty.shortRev;
                  }
              );

          ggmlCpu =
            {
              x86_64-linux = "-DGGML_NATIVE=OFF -DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON";
            }
            .${system} or "-DGGML_NATIVE=OFF";

          buildTools = [
            toolchain
            pkgs.coreutils
            pkgs.gnumake
            pkgs.protobuf
            pkgs.stdenv.cc
          ]
          ++ speechBuild
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            pkgs.gnused
            pkgs.stdenv.cc.bintools.bintools
          ];

          macBuildEnv = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
            export SDKROOT="${pkgs.apple-sdk.sdkroot}"
            export LIBRARY_PATH="${pkgs.lib.makeLibraryPath [ pkgs.libiconv ]}''${LIBRARY_PATH:+:$LIBRARY_PATH}"
          '';

          testRun = "-p lunetad -p luneta-protocol -p luneta-mux";

          daemon = {
            pname = "lunetad";
            inherit version src;
            strictDeps = true;

            cargoExtraArgs = "--locked -p lunetad -p luneta-mux";

            nativeBuildInputs = [ pkgs.protobuf ] ++ speechBuild;
            buildInputs = speechLibs ++ [ libghosttyVt.dev ];
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;

            nativeCheckInputs = [
              pkgs.cargo-nextest
              pkgs.fish
              pkgs.git
              pkgs.nodejs
              pkgs.openssh
              pkgs.procps
            ];

            doCheck = !pkgs.stdenv.hostPlatform.isDarwin;
          };

          daemonDeps = craneLib.buildDepsOnly daemon;

          lunetad = craneLib.buildPackage (
            daemon
            // {
              src = testSrc;
              cargoArtifacts = daemonDeps;

              NEXTEST_SHOW_PROGRESS = "counter";
              checkPhaseCargoCommand = ''
                cargo nextest run --cargo-profile "$CARGO_PROFILE" --profile ci \
                  --test-threads "$NIX_BUILD_CORES" --locked ${testRun}
                cargoWithProfile test --doc --locked ${testRun}
              '';

              postInstall = ''
                install -Dm444 ${./shell/luneta.fish} $out/share/fish/vendor_conf.d/luneta.fish
              '';

              meta = {
                description = "The luneta daemon, its CLI, and luneta's own multiplexer";
                homepage = "https://github.com/lorenzolfm/luneta";
                license = pkgs.lib.licenses.mit;
                mainProgram = "lunetad";
                platforms = systems;
              };
            }
            // pkgs.lib.optionalAttrs (self ? rev) {
              LUNETA_BUILT = toString self.lastModified;
            }
          );

          clippyRun = "--all-targets --workspace";

          clippy = craneLib.cargoClippy (
            daemon
            // {
              src = lintSrc;
              cargoArtifacts = daemonDeps;
              cargoExtraArgs = "--locked";
              cargoClippyExtraArgs = "${clippyRun} -- -D warnings";
              doCheck = false;
              doInstallCargoArtifacts = false;
            }
          );

          dev = import ./nix/dev {
            inherit
              pkgs
              toolchain
              buildTools
              clippyRun
              testRun
              ;
            testTools = daemon.nativeCheckInputs;
            compileEnv = ''
              export TRANSCRIBE_CMAKE_ARGS="${ggmlCpu}"
              export PKG_CONFIG_PATH="${
                pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" speechLibs
              }:${libghosttyVt.dev}/lib/pkgconfig''${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
              ${macBuildEnv}
            '';
          };

          deny = craneLib.cargoDeny {
            inherit (daemon) pname version;
            src = denySrc;
            cargoDenyExtraArgs = "--offline --locked";
          };

          nixFiles = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.fileFilter (file: file.hasExt "nix") ./.;
          };

          nixLint =
            tool: args:
            pkgs.runCommand "luneta-${tool.pname}" { } ''
              cd ${nixFiles}
              ${pkgs.lib.getExe tool} ${args}
              touch "$out"
            '';
        in
        {
          inherit dev lunetad;

          checks = {
            inherit clippy deny;
            statix = nixLint pkgs.statix "check .";
            deadnix = nixLint pkgs.deadnix "--hidden --fail .";
            comments =
              let
                comments = import ./nix/comments.nix { inherit pkgs; };
              in
              pkgs.runCommand "luneta-comments" { } ''
                cd ${./.}
                find . -type f -print0 | ${pkgs.lib.getExe comments}
                touch "$out"
              '';
          };

          shell = pkgs.mkShell {
            packages = buildTools ++ [
              dev
              pkgs.cargo-nextest
              pkgs.fish
              pkgs.nodejs
            ];
            buildInputs = speechLibs ++ [ libghosttyVt.dev ];
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;
            LUNETA_TEST_RUN = testRun;

            shellHook = ''
              echo "luneta — rust $(rustc --version | cut -d' ' -f2); run 'dev help' for the command set"
            '';
          };
        };

      bySystem = nixpkgs.lib.genAttrs systems build;
    in
    {
      devShells = nixpkgs.lib.mapAttrs (_: built: { default = built.shell; }) bySystem;

      packages = nixpkgs.lib.mapAttrs (_: built: {
        inherit (built) dev lunetad;
        default = built.lunetad;
      }) bySystem;

      checks = nixpkgs.lib.mapAttrs (_: built: built.checks) bySystem;
    };
}
