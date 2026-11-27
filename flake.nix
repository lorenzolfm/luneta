{
  description = "lune — the terminals of a worktree beside a sidebar of every project and the agents in them, its daemon, and its own multiplexer";

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
      toolchainToml = builtins.fromTOML (builtins.readFile ./rust-toolchain.toml);
      inherit (cargoToml.workspace.package) version;
      rustVersion =
        cargoToml.workspace.package.rust-version
          or (throw "Cargo.toml has no [workspace.package].rust-version");
      toolchainChannel =
        toolchainToml.toolchain.channel or (throw "rust-toolchain.toml has no [toolchain].channel");
      workspaceMembers = cargoToml.workspace.members or [ ];
      workspaceExclude = cargoToml.workspace.exclude or [ ];

      build =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          stableChannel =
            builtins.isString toolchainChannel
            && builtins.match "[0-9]+\\.[0-9]+\\.[0-9]+" toolchainChannel != null;
          crateDirs = builtins.attrNames (
            pkgs.lib.filterAttrs (_: kind: kind == "directory") (builtins.readDir ./crates)
          );
          missingRustVersion = builtins.filter (
            dir:
            let
              manifestPath = ./crates + "/${dir}/Cargo.toml";
              manifest = builtins.fromTOML (builtins.readFile manifestPath);
            in
            !(builtins.pathExists manifestPath)
            || (manifest.package.rust-version or null) != { workspace = true; }
          ) crateDirs;
          noRepoCargoConfig =
            !(builtins.pathExists ./.cargo/config.toml) && !(builtins.pathExists ./.cargo/config);
          toolchainChecks = [
            {
              ok = stableChannel;
              message = "rust-toolchain.toml pins ${builtins.toJSON toolchainChannel}, not a stable X.Y.Z release";
            }
            {
              ok = toolchainChannel == rustVersion;
              message = "rust-toolchain.toml pins ${builtins.toJSON toolchainChannel}, but Cargo.toml sets rust-version = ${builtins.toJSON rustVersion}";
            }
            {
              ok = workspaceMembers == [ "crates/*" ] && workspaceExclude == [ ];
              message = "Cargo.toml workspace members changed; update flake.nix's rust-version inheritance check";
            }
            {
              ok = missingRustVersion == [ ];
              message = "crates missing rust-version.workspace = true: ${pkgs.lib.concatStringsSep ", " missingRustVersion}";
            }
            {
              ok = noRepoCargoConfig;
              message = "a tracked .cargo/config(.toml) is ignored by flake.nix's cargoFiles while dev and plain cargo read it; remove it or wire it into cargoFiles and ci.yml";
            }
          ];
          toolchain = pkgs.lib.foldr (
            check: next: pkgs.lib.throwIfNot check.ok check.message next
          ) (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml) toolchainChecks;

          localToolchain =
            if pkgs.stdenv.hostPlatform.isDarwin then
              pkgs.symlinkJoin {
                name = "${toolchain.name}-unwrapped-cargo";
                paths = [ toolchain ];
                postBuild = ''
                  rm "$out/bin/cargo"
                  ln -s ${toolchain.availableComponents.cargo}/bin/cargo "$out/bin/cargo"
                '';
              }
            else
              toolchain;

          craneLib = ((crane.mkLib pkgs).overrideToolchain toolchain).overrideScope (
            _: prev: { writeTOML = prev.writeTOMLViaRemarshal; }
          );

          cargoFiles = pkgs.lib.fileset.unions [
            ./crates
            ./proto
            ./skills/lune/SKILL.md
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
              ./nix/dev/installed.nix
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
            localToolchain
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

          testRun = "-p lune -p luned -p lune-diff -p lune-domain -p lune-protocol -p lune-mux -p lune-phone -p lune-waits";

          nextest = import ./nix/nextest.nix { inherit pkgs testRun; };

          daemon = {
            pname = "luned";
            inherit version src;
            strictDeps = true;

            cargoExtraArgs = "--locked -p lune -p luned -p lune-mux";

            nativeBuildInputs = [ pkgs.protobuf ] ++ speechBuild;
            buildInputs = speechLibs ++ [ libghosttyVt.dev ];
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;

            nativeCheckInputs = [
              pkgs.cargo-nextest
              pkgs.git
              pkgs.nodejs
              pkgs.openssh
              pkgs.procps
            ];
          };

          checked = daemon // {
            CARGO_PROFILE = "checks";
          };

          daemonDeps = craneLib.buildDepsOnly checked;

          luned =
            let
              released = daemon // {
                doCheck = false;
              };
            in
            craneLib.buildPackage (
              released
              // {
                cargoArtifacts = craneLib.buildDepsOnly released;

                meta = {
                  description = "The lune daemon, its CLI, and lune's own multiplexer";
                  homepage = "https://github.com/lorenzolfm/lune";
                  license = pkgs.lib.licenses.mit;
                  mainProgram = "luned";
                  platforms = systems;
                };
              }
              // pkgs.lib.optionalAttrs (self ? rev) {
                LUNE_BUILT = toString self.lastModified;
              }
            );

          tested = craneLib.mkCargoDerivation (
            builtins.removeAttrs checked [ "cargoExtraArgs" ]
            // {
              pnameSuffix = "-tested";
              src = testSrc;
              cargoArtifacts = daemonDeps;
              doCheck = true;
              doInstallCargoArtifacts = false;

              LUNE_BUILT = "1";
              NEXTEST_SHOW_PROGRESS = "counter";
              NEXTEST_DOUBLE_SPAWN = "0";

              preBuild = ''
                export PATH="${localToolchain}/bin:$PATH"
              '';
              buildPhaseCargoCommand = ''
                ${pkgs.lib.getExe nextest} --cargo-profile "$CARGO_PROFILE" --no-run
              '';
              checkPhaseCargoCommand = ''
                ${pkgs.lib.getExe nextest} --cargo-profile "$CARGO_PROFILE" --profile ci \
                  --test-threads "$NIX_BUILD_CORES"
                cargoWithProfile test --doc --locked ${testRun}
              '';
            }
          );

          clippyRun = "--all-targets --workspace";

          clippy = craneLib.cargoClippy (
            checked
            // {
              src = lintSrc;
              cargoArtifacts = daemonDeps;
              cargoExtraArgs = "--locked";
              cargoClippyExtraArgs = "${clippyRun} --exclude lune-phone-bindgen -- -D warnings";
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
              nextest
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
            pkgs.runCommand "lune-${tool.pname}" { } ''
              cd ${nixFiles}
              ${pkgs.lib.getExe tool} ${args}
              touch "$out"
            '';

          terminal-browser = import ./nix/terminal-browser.nix { inherit pkgs; };
        in
        {
          inherit dev luned terminal-browser;

          checks = {
            inherit clippy deny;
            luned = tested;
            statix = nixLint pkgs.statix "check .";
            swift =
              let
                swift = import ./nix/swift.nix { inherit pkgs; };
              in
              pkgs.runCommand "lune-swift"
                {
                  src = pkgs.lib.fileset.toSource {
                    root = ./.;
                    fileset = pkgs.lib.fileset.unions [
                      (pkgs.lib.fileset.fileFilter (file: file.hasExt "swift") ./ios)
                      ./ios/.swiftlint.yml
                    ];
                  };
                }
                ''
                  cd "$src"
                  find . -name '*.swift' -print0 | ${pkgs.findutils}/bin/xargs -0 -r ${pkgs.lib.getExe swift.check}
                  touch "$out"
                '';
            deadnix = nixLint pkgs.deadnix "--hidden --fail .";
            profiles =
              let
                failures = import ./nix/profiles.nix { root = ./.; };
              in
              pkgs.runCommand "lune-profiles" { } (
                if failures == [ ] then
                  ''touch "$out"''
                else
                  ''
                    printf '%s\n' ${pkgs.lib.escapeShellArgs failures} >&2
                    exit 1
                  ''
              );
            comments =
              let
                comments = import ./nix/comments.nix { inherit pkgs; };
              in
              pkgs.runCommand "lune-comments" { } ''
                cd ${./.}
                find . -type f -print0 | ${pkgs.lib.getExe comments}
                touch "$out"
              '';
            typescript =
              let
                typescript = import ./nix/typescript.nix { inherit pkgs; };
              in
              pkgs.runCommand "lune-typescript"
                {
                  src = pkgs.lib.fileset.toSource {
                    root = ./.;
                    fileset = pkgs.lib.fileset.fileFilter (file: file.hasExt "ts") ./crates;
                  };
                }
                ''
                  cd "$src"
                  find . -name '*.ts' -print0 | ${pkgs.findutils}/bin/xargs -0 -r ${pkgs.lib.getExe typescript}
                  touch "$out"
                '';
          };

          shell = pkgs.mkShell {
            packages = buildTools ++ [
              dev
              nextest
              pkgs.cargo-nextest
              pkgs.nodejs
            ];
            buildInputs = speechLibs ++ [ libghosttyVt.dev ];
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;
            LUNE_TEST_RUN = testRun;

            shellHook = ''
              echo "lune — rust $(rustc --version | cut -d' ' -f2); run 'dev help' for the command set"
            '';
          };
        };

      bySystem = nixpkgs.lib.genAttrs systems build;
    in
    {
      devShells = nixpkgs.lib.mapAttrs (_: built: { default = built.shell; }) bySystem;

      packages = nixpkgs.lib.mapAttrs (_: built: {
        inherit (built) dev luned terminal-browser;
        default = built.luned;
      }) bySystem;

      checks = nixpkgs.lib.mapAttrs (_: built: built.checks) bySystem;
    };
}
