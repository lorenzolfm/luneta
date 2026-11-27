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
      systemConfigs = {
        x86_64-linux = {
          crossTarget = "aarch64-apple-darwin";
          ggmlCpu = "-DGGML_NATIVE=OFF -DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON";
        };
        aarch64-linux = {
          crossTarget = "aarch64-apple-darwin";
          ggmlCpu = "-DGGML_NATIVE=OFF";
        };
        aarch64-darwin = {
          crossTarget = "x86_64-unknown-linux-gnu";
          ggmlCpu = "-DGGML_NATIVE=OFF";
        };
      };

      systems = builtins.attrNames systemConfigs;

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
          cfg = systemConfigs.${system};
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
          checksProfile = "checks";
          profileFailures = import ./nix/profiles.nix {
            root = ./.;
            inherit checksProfile;
          };
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
            {
              ok = profileFailures == [ ];
              message = pkgs.lib.concatStringsSep "\n" profileFailures;
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

          denySrc = craneLib.mkDummySrc {
            inherit src;
            cleanCargoTomlFilter = _: true;
            extraDummyScript = ''
              cp ${./deny.toml} "$out/deny.toml"
            '';
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

          inherit (cfg) ggmlCpu;

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

          daemonPackages = "-p lune -p luned -p lune-mux";

          testTools = [
            pkgs.cargo-nextest
            pkgs.git
            pkgs.nodejs
            pkgs.openssh
            pkgs.procps
          ];

          cargoVendorDir = craneLib.vendorCargoDeps { inherit src; };

          daemon = {
            pname = "luned";
            inherit version src cargoVendorDir;
            strictDeps = true;

            nativeBuildInputs = [ pkgs.protobuf ] ++ speechBuild;
            buildInputs = speechLibs ++ [ libghosttyVt.dev ];
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;
          };

          checksArgs = daemon // {
            CARGO_PROFILE = checksProfile;
          };

          checksDeps = craneLib.buildDepsOnly (
            checksArgs
            // {
              pname = "luned-checks";
              cargoExtraArgs = "--locked ${daemonPackages}";
              doCheck = true;
            }
          );

          releaseArgs = daemon // {
            CARGO_PROFILE = "release";
            cargoExtraArgs = "--locked ${daemonPackages}";
            doCheck = false;
          };

          releaseDeps = craneLib.buildDepsOnly releaseArgs;

          luned = craneLib.buildPackage (
            releaseArgs
            // {
              cargoArtifacts = releaseDeps;

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
            checksArgs
            // {
              pnameSuffix = "-tested";
              src = testSrc;
              cargoArtifacts = checksDeps;
              doCheck = false;
              doInstallCargoArtifacts = false;

              LUNE_BUILT = "1";
              NEXTEST_SHOW_PROGRESS = "counter";
              NEXTEST_DOUBLE_SPAWN = "0";

              buildPhaseCargoCommand = ''
                if [ "''${CARGO_PROFILE:-}" != "${checksProfile}" ]; then
                  echo "luned-tested: expected CARGO_PROFILE=${checksProfile}, got ''${CARGO_PROFILE:-<unset>}" >&2
                  exit 1
                fi
                case "''${NIX_BUILD_CORES:-}" in
                  "" | *[!0-9]* | 0*)
                    echo "luned-tested: NIX_BUILD_CORES must be a positive integer, got ''${NIX_BUILD_CORES:-<unset>}" >&2
                    exit 1
                    ;;
                  [1-7])
                    test_threads=$((NIX_BUILD_CORES * 3))
                    ;;
                  *)
                    test_threads=24
                    ;;
                esac
                export PATH="${localToolchain}/bin:${pkgs.lib.makeBinPath testTools}:$PATH"
                ${pkgs.lib.getExe nextest} --cargo-profile "${checksProfile}" --profile ci \
                  --test-threads "$test_threads"
                cargoWithProfile test --doc --locked ${testRun}
              '';
            }
          );

          clippyRun = "--all-targets --workspace";
          vendoredClippyRun = "${clippyRun} --exclude lune-phone-bindgen";

          clippy = craneLib.cargoClippy (
            checksArgs
            // {
              src = lintSrc;
              cargoArtifacts = checksDeps;
              cargoExtraArgs = "--locked";
              cargoClippyExtraArgs = "${vendoredClippyRun} -- -D warnings";
              postBuild = ''
                cargoWithProfile clippy --locked ${daemonPackages} -- -D warnings
              '';
              doCheck = false;
              doInstallCargoArtifacts = false;
            }
          );

          hostTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;

          skipNative =
            triple:
            ''
              [target.${triple}.transcribe]
              rustc-link-lib = ["transcribe"]
              [target.${triple}.alsa]
              rustc-link-lib = ["asound"]
              [target.${triple}.ghostty-vt]
              rustc-link-lib = ["ghostty-vt"]
            '';

          hostBuildScriptsSkipped = pkgs.writeText "clippy-host.toml" (skipNative hostTarget);

          cross =
            let
              target =
                pkgs.lib.throwIfNot (cfg.crossTarget != hostTarget)
                  "clippy-cross target ${cfg.crossTarget} matches host ${hostTarget}"
                  cfg.crossTarget;
            in
            {
              inherit target;
              toolchain = toolchain.override { targets = [ target ]; };
              buildScriptsSkipped = pkgs.writeText "clippy-cross.toml" (skipNative target);
            };

          craneCross = craneLib.overrideToolchain cross.toolchain;

          clippyCrossArgs = {
            pname = "luned-cross";
            inherit version cargoVendorDir;
            src = lintSrc;
            strictDeps = true;

            CARGO_PROFILE = "dev";
            cargoExtraArgs = "--locked --config ${cross.buildScriptsSkipped} --target ${cross.target}";

            nativeBuildInputs = [ pkgs.protobuf ];
            doCheck = false;
          };

          clippyCrossDeps = craneCross.buildDepsOnly (
            clippyCrossArgs
            // {
              buildPhaseCargoCommand = "cargoWithProfile check ${clippyCrossArgs.cargoExtraArgs} ${vendoredClippyRun}";
            }
          );

          clippyCross = craneCross.cargoClippy (
            clippyCrossArgs
            // {
              cargoArtifacts = clippyCrossDeps;
              cargoClippyExtraArgs = "${vendoredClippyRun} -- -D warnings";
              postBuild = ''
                cargoWithProfile clippy ${clippyCrossArgs.cargoExtraArgs} ${daemonPackages} -- -D warnings
              '';
              doInstallCargoArtifacts = false;
            }
          );

          clippyReleaseArgs = {
            pname = "luned-release-clippy";
            inherit version cargoVendorDir;
            src = lintSrc;
            strictDeps = true;

            CARGO_PROFILE = "release";
            cargoExtraArgs = "--locked --config ${hostBuildScriptsSkipped} ${daemonPackages}";

            nativeBuildInputs = [ pkgs.protobuf ];
            doCheck = false;
          };

          clippyReleaseDeps = craneLib.buildDepsOnly (
            clippyReleaseArgs
            // {
              buildPhaseCargoCommand = "cargoWithProfile check ${clippyReleaseArgs.cargoExtraArgs}";
            }
          );

          clippyRelease = craneLib.cargoClippy (
            clippyReleaseArgs
            // {
              cargoArtifacts = clippyReleaseDeps;
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
            }
          );

          clippyCrossReleaseArgs = {
            pname = "luned-cross-release-clippy";
            inherit version cargoVendorDir;
            src = lintSrc;
            strictDeps = true;

            CARGO_PROFILE = "release";
            cargoExtraArgs = "--locked --config ${cross.buildScriptsSkipped} --target ${cross.target} ${daemonPackages}";

            nativeBuildInputs = [ pkgs.protobuf ];
            doCheck = false;
          };

          clippyCrossReleaseDeps = craneCross.buildDepsOnly (
            clippyCrossReleaseArgs
            // {
              buildPhaseCargoCommand = "cargoWithProfile check ${clippyCrossReleaseArgs.cargoExtraArgs}";
            }
          );

          clippyCrossRelease = craneCross.cargoClippy (
            clippyCrossReleaseArgs
            // {
              cargoArtifacts = clippyCrossReleaseDeps;
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
            }
          );

          dev = import ./nix/dev {
            inherit
              pkgs
              toolchain
              buildTools
              macBuildEnv
              hostBuildScriptsSkipped
              cross
              clippyRun
              daemonPackages
              testRun
              nextest
              testTools
              ;
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
            inherit cargoVendorDir;
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

          lintChecks = {
            inherit clippy deny;
            clippy-release = clippyRelease;
            clippy-cross = clippyCross;
            clippy-cross-release = clippyCrossRelease;
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
                failures = import ./nix/profiles.nix {
                  root = ./.;
                  inherit checksProfile;
                  derivationProfiles = {
                    checksDeps = checksDeps.CARGO_PROFILE or null;
                    tested = tested.CARGO_PROFILE or null;
                    clippy = clippy.CARGO_PROFILE or null;
                    clippyRelease = clippyRelease.CARGO_PROFILE or null;
                    clippyCrossRelease = clippyCrossRelease.CARGO_PROFILE or null;
                    releaseDeps = releaseDeps.CARGO_PROFILE or null;
                    luned = luned.CARGO_PROFILE or null;
                  };
                };
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
            ios-inputs =
              let
                failures = import ./nix/ios-inputs.nix { root = ./.; };
              in
              pkgs.runCommand "lune-ios-inputs" { } (
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

          lints = pkgs.linkFarm "lune-lints" (
            pkgs.lib.mapAttrsToList (name: path: { inherit name path; }) lintChecks
          );
        in
        {
          inherit dev luned terminal-browser;

          checks = lintChecks // {
            inherit lints;
            luned = tested;
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
