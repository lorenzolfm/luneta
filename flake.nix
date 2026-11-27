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
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      ghostty,
      ...
    }:
    let
      systemConfigs = {
        x86_64-linux = {
          runner = "ubuntu-latest";
          crossTarget = "aarch64-apple-darwin";
          ggmlCpu = "-DGGML_NATIVE=OFF -DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON";
        };
        aarch64-linux = {
          runner = "ubuntu-24.04-arm";
          crossTarget = "aarch64-apple-darwin";
          ggmlCpu = "-DGGML_NATIVE=OFF";
        };
        aarch64-darwin = {
          runner = "macos-latest";
          crossTarget = "x86_64-unknown-linux-gnu";
          ggmlCpu = "-DGGML_NATIVE=OFF";
        };
      };

      ciRunners = nixpkgs.lib.mapAttrs (_: cfg: cfg.runner) systemConfigs;
      systems = builtins.attrNames systemConfigs;

      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      cargoLock = builtins.fromTOML (builtins.readFile ./Cargo.lock);
      toolchainToml = builtins.fromTOML (builtins.readFile ./rust-toolchain.toml);
      nextestToml = builtins.fromTOML (builtins.readFile ./.config/nextest.toml);
      ciProfile = nextestToml.profile.ci or (throw ".config/nextest.toml has no [profile.ci]");
      ciRetriesRaw = ciProfile.retries or 0;
      ciRetries =
        if builtins.isInt ciRetriesRaw then
          ciRetriesRaw
        else
          ciRetriesRaw.count or (throw ".config/nextest.toml [profile.ci].retries has no count");
      ciJunitPath =
        (ciProfile.junit or { }).path or (throw ".config/nextest.toml has no [profile.ci.junit].path");
      inherit (cargoToml.workspace.package) version;
      rustVersion =
        cargoToml.workspace.package.rust-version
          or (throw "Cargo.toml has no [workspace.package].rust-version");
      toolchainChannel =
        toolchainToml.toolchain.channel or (throw "rust-toolchain.toml has no [toolchain].channel");
      workspaceMembers = cargoToml.workspace.members or [ ];
      workspaceExclude = cargoToml.workspace.exclude or [ ];

      validSriHash =
        hash: builtins.isString hash && builtins.match "sha256-[A-Za-z0-9+/]{43}=" hash != null;

      libghosttyRsSource = "git+https://github.com/uzaaft/libghostty-rs?rev=cd4d497e8c698b27054a677ce19d1bd1a1a4d8bf#cd4d497e8c698b27054a677ce19d1bd1a1a4d8bf";
      gitDeps = {
        ${libghosttyRsSource} = {
          hash = "sha256-VkLyG0Y3V8ugEl2emA7Zpe0StQnwqu8Ay2xMQWr8Kn8=";
          ghosttyRev = "108cacb17ed8990840cd14abfce43d8ed8503ca1";
        };
        "git+https://github.com/lorenzolfm/wt?rev=b73f7d36d8f6f6c9301da9568de82ec8bd917cce#b73f7d36d8f6f6c9301da9568de82ec8bd917cce" =
          {
            hash = "sha256-KYTyp97dAhus2mzuvsvX4JbRilIbucNIBe7xR2xf+BE=";
          };
      };
      gitHashes = builtins.mapAttrs (_: dep: dep.hash) gitDeps;
      gitSources = nixpkgs.lib.unique (
        builtins.filter (nixpkgs.lib.hasPrefix "git") (map (crate: crate.source or "") cargoLock.package)
      );
      encodedGitSources = builtins.filter (
        source: builtins.match ".*%[0-9A-Fa-f]{2}.*" source != null
      ) gitSources;
      unhashed = builtins.filter (
        source: !(gitDeps ? ${source}) || !(validSriHash (gitDeps.${source}.hash or null))
      ) gitSources;
      staleGitHashes = builtins.filter (source: !(builtins.elem source gitSources)) (
        builtins.attrNames gitDeps
      );
      gitHashChecks = [
        {
          ok = encodedGitSources == [ ];
          message = "Cargo.lock has percent-encoded git sources not handled by gitDeps: ${nixpkgs.lib.concatStringsSep ", " encodedGitSources}";
        }
        {
          ok = unhashed == [ ];
          message = "gitDeps has no valid SRI hash for ${nixpkgs.lib.concatStringsSep ", " unhashed}, so every evaluation would clone it";
        }
        {
          ok = staleGitHashes == [ ];
          message = "gitDeps has unused entries not in Cargo.lock: ${nixpkgs.lib.concatStringsSep ", " staleGitHashes}";
        }
      ];

      ghosttySysMatches = builtins.filter (crate: crate.name == "libghostty-vt-sys") cargoLock.package;
      ghosttySys =
        if builtins.length ghosttySysMatches == 1 then
          builtins.head ghosttySysMatches
        else
          throw "Cargo.lock must have exactly one libghostty-vt-sys, found ${toString (builtins.length ghosttySysMatches)}";
      ghosttySysRev =
        gitDeps.${ghosttySys.source}.ghosttyRev
          or (throw "gitDeps has no ghosttyRev for libghostty-vt-sys from ${ghosttySys.source}");

      cachedPathsMax = 32;

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

          baseCraneLib = ((crane.mkLib pkgs).overrideToolchain toolchain).overrideScope (
            _: prev: {
              writeTOML = prev.writeTOMLViaRemarshal;
              downloadCargoPackageFromGit =
                args:
                pkgs.lib.throwIfNot (args ? hash && validSriHash args.hash)
                  "downloadCargoPackageFromGit requires an SRI hash for ${args.git or "<unknown>"}"
                  (prev.downloadCargoPackageFromGit args);
            }
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

          linkedVendor = nixpkgs.lib.foldr (check: next: nixpkgs.lib.throwIfNot check.ok check.message next) (
            baseCraneLib.vendorCargoDeps
              {
                cargoLockParsed = cargoLock;
                outputHashes = gitHashes;
              }
          ) gitHashChecks;
          cargoVendorDir = pkgs.runCommand "lune-cargo-vendor" { allowedReferences = [ "out" ]; } ''
            cp -rL "${linkedVendor}" "$out"
            chmod u+w "$out" "$out/config.toml"
            substituteInPlace "$out/config.toml" --replace-fail "${linkedVendor}" "$out"
            chmod a-w "$out" "$out/config.toml"
          '';

          craneLib = baseCraneLib.overrideScope (
            _: _: {
              vendorCargoDeps = _: throw "pass cargoVendorDir directly; unbundled vendorCargoDeps is forbidden";
            }
          );

          speechBuild = [
            pkgs.cmake
            pkgs.pkg-config
          ];
          speechLibs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.alsa-lib ];

          libghosttyVt =
            pkgs.lib.throwIfNot (ghostty.rev == ghosttySysRev)
              "the ghostty input is not ${ghosttySysRev}, which libghostty-vt-sys binds"
              (
                (pkgs.callPackage "${ghostty}/nix/libghostty-vt.nix" {
                  optimize = "ReleaseSafe";
                  revision = ghostty.shortRev;
                }).overrideAttrs
                  (
                    finalAttrs: prev: {
                      zigBuildFlags = prev.zigBuildFlags ++ [ "-Dstrip=true" ];
                      disallowedRequisites = [
                        finalAttrs.deps
                        finalAttrs.src
                        pkgs.zig_0_17
                        pkgs.git
                      ];
                      postFixup = (prev.postFixup or "") + ''
                        "$STRIP" -S "$dev/lib/libghostty-vt.a"
                        "$RANLIB" "$dev/lib/libghostty-vt.a"
                      '';
                    }
                  )
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
          daemon = {
            pname = "luned";
            inherit version src cargoVendorDir;
            strictDeps = true;

            nativeBuildInputs = [ pkgs.protobuf ] ++ speechBuild;
            buildInputs = speechLibs ++ [ libghosttyVt.dev ];
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;
          };

          scrubCargoDeps =
            label: depsToolchains:
            ''
              targetDir="''${CARGO_TARGET_DIR:-target}"
              rm -f "$targetDir/.rustc_info.json"
              find "$targetDir" -path '*/build/transcribe-cpp-sys-*/out/build' -type d -prune -exec rm -rf -- {} +
            ''
            + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              source ${pkgs.darwin.signingUtils}
              for depToolchain in ${pkgs.lib.escapeShellArgs depsToolchains}; do
                toolchainHash="$(basename "$depToolchain")"
                toolchainHash="''${toolchainHash%%-*}"
                while IFS= read -r -d "" file; do
                  if [[ -f $file && ! -L $file ]] && checkRequiresSignature "$file"; then
                    chmod u+w "$file"
                    "$STRIP" -S "$file"
                    sign "$file"
                  fi
                done < <(grep -rlFZ -- "$toolchainHash" "$targetDir" || true)
              done
            ''
            + ''
              for depToolchain in ${pkgs.lib.escapeShellArgs depsToolchains}; do
                toolchainHash="$(basename "$depToolchain")"
                toolchainHash="''${toolchainHash%%-*}"
                found=0
                grep -rlF -- "$toolchainHash" "$targetDir" >&2 || found=$?
                if (( found == 0 )); then
                  echo "${label}: $targetDir still references $depToolchain before compression" >&2
                  exit 1
                elif (( found != 1 )); then
                  exit "$found"
                fi
              done
            '';
          checksArgs = daemon // {
            CARGO_PROFILE = checksProfile;
          };

          checksDeps = craneLib.buildDepsOnly (
            checksArgs
            // {
              pname = "luned-checks";
              cargoExtraArgs = "--locked ${daemonPackages}";
              doCheck = true;
              disallowedRequisites = [
                toolchain
              ];
              preInstall = scrubCargoDeps "checksDeps" [ toolchain ];
            }
          );

          releaseArgs = daemon // {
            CARGO_PROFILE = "release";
            cargoExtraArgs = "--locked ${daemonPackages}";
            doCheck = false;
          };

          releaseDeps = craneLib.buildDepsOnly (
            releaseArgs
            // {
              disallowedRequisites = [
                toolchain
              ];
              preInstall = scrubCargoDeps "releaseDeps" [ toolchain ];
            }
          );

          lunedDisallowedRequisites = [
            toolchain
            cargoVendorDir
            src
            pkgs.protobuf
            pkgs.cmake
          ]
          ++ testTools;

          luned = craneLib.buildPackage (
            releaseArgs
            // {
              cargoArtifacts = releaseDeps;
              disallowedRequisites = lunedDisallowedRequisites;

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
              disallowedRequisites = [
                toolchain
                cargoVendorDir
                src
                testSrc
                pkgs.protobuf
                pkgs.cmake
              ];

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
              postBuild = ''
                junit="''${CARGO_TARGET_DIR:-target}/nextest/ci/"${pkgs.lib.escapeShellArg ciJunitPath}
                test -f "$junit"
                cp "$junit" "$TMPDIR/luned-junit.xml"
              '';
              postInstall = ''
                install -Dm444 "$TMPDIR/luned-junit.xml" "$out/share/luned/junit.xml"
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
              allowedReferences = [ ];
            }
          );

          hostTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;

          skipNative = triple: ''
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
              target = pkgs.lib.throwIfNot (
                cfg.crossTarget != hostTarget
              ) "clippy-cross target ${cfg.crossTarget} matches host ${hostTarget}" cfg.crossTarget;
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
              disallowedRequisites = [
                toolchain
                cross.toolchain
              ];
              preInstall = scrubCargoDeps "clippyCrossDeps" [
                toolchain
                cross.toolchain
              ];
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
              allowedReferences = [ ];
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
              disallowedRequisites = [
                toolchain
              ];
              preInstall = scrubCargoDeps "clippyReleaseDeps" [ toolchain ];
            }
          );

          clippyRelease = craneLib.cargoClippy (
            clippyReleaseArgs
            // {
              cargoArtifacts = clippyReleaseDeps;
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              allowedReferences = [ ];
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
              disallowedRequisites = [
                toolchain
                cross.toolchain
              ];
              preInstall = scrubCargoDeps "clippyCrossReleaseDeps" [
                toolchain
                cross.toolchain
              ];
            }
          );

          clippyCrossRelease = craneCross.cargoClippy (
            clippyCrossReleaseArgs
            // {
              cargoArtifacts = clippyCrossReleaseDeps;
              cargoClippyExtraArgs = "-- -D warnings";
              doInstallCargoArtifacts = false;
              allowedReferences = [ ];
            }
          );
          deny = craneLib.cargoDeny {
            inherit (daemon) pname version cargoVendorDir;
            src = denySrc;
            cargoDenyExtraArgs = "--offline --locked";
            allowedReferences = [ ];
          };

          markerCheck =
            name: attrs: command:
            pkgs.runCommand name ({ allowedReferences = [ ]; } // attrs) command;

          nixFiles = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.fileFilter (file: file.hasExt "nix") ./.;
          };

          nixLint =
            tool: args:
            markerCheck "lune-${tool.pname}" { } ''
              cd ${nixFiles}
              ${pkgs.lib.getExe tool} ${args}
              touch "$out"
            '';

          flakyReport = import ./nix/flaky-report.nix {
            inherit pkgs;
            maxAttempts = ciRetries + 1;
            derivationName = tested.pname;
          };
          lintChecks = {
            inherit clippy deny;
            clippy-release = clippyRelease;
            clippy-cross = clippyCross;
            clippy-cross-release = clippyCrossRelease;
            flaky-report = flakyReport.check;
            statix = nixLint pkgs.statix "check .";
            swift =
              let
                swift = import ./nix/swift.nix { inherit pkgs; };
              in
              markerCheck "lune-swift"
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
              markerCheck "lune-profiles" { } (
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
              markerCheck "lune-ios-inputs" { } (
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
              markerCheck "lune-comments" { } ''
                cd ${./.}
                find . -type f -print0 | ${pkgs.lib.getExe comments}
                touch "$out"
              '';
            typescript =
              let
                typescript = import ./nix/typescript.nix { inherit pkgs; };
              in
              markerCheck "lune-typescript"
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

          cachedArtifacts = [
            cargoVendorDir
            checksDeps
            releaseDeps
            luned
            clippyCrossDeps
            clippyReleaseDeps
            clippyCrossReleaseDeps
            libghosttyVt
          ];

          cachedChecks = lintChecks // {
            luned = tested;
          };

          cachedDerivations = cachedArtifacts ++ builtins.attrValues cachedChecks;
          rawCachedPaths = nixpkgs.lib.concatMap (
            artifact: map (output: artifact.${output}.outPath) artifact.outputs
          ) cachedDerivations;
          cachedChecksList = [
            {
              ok = builtins.all nixpkgs.lib.isDerivation cachedDerivations;
              message = "cached.${system} contains a non-derivation";
            }
            {
              ok = rawCachedPaths != [ ];
              message = "cached.${system} is empty";
            }
            {
              ok = builtins.length rawCachedPaths <= cachedPathsMax;
              message = "cached.${system} has ${toString (builtins.length rawCachedPaths)} paths, exceeding ${toString cachedPathsMax}";
            }
            {
              ok = builtins.length rawCachedPaths == builtins.length (nixpkgs.lib.unique rawCachedPaths);
              message = "cached.${system} contains duplicate store paths";
            }
            {
              ok = builtins.all (
                path: nixpkgs.lib.hasPrefix "/nix/store/" path && !(nixpkgs.lib.hasSuffix ".drv" path)
              ) rawCachedPaths;
              message = "cached.${system} contains a non-output store path";
            }
          ];
          cachedPaths = nixpkgs.lib.foldr (
            check: next: nixpkgs.lib.throwIfNot check.ok check.message next
          ) rawCachedPaths cachedChecksList;

          cachedCheck = markerCheck "lune-cached" { } (
            builtins.seq (builtins.deepSeq cachedPaths null) ''
              touch "$out"
            ''
          );

          lints = pkgs.linkFarm "lune-lints" (
            pkgs.lib.mapAttrsToList (name: path: { inherit name path; }) (
              lintChecks // { cached = cachedCheck; }
            )
          );

          checks = cachedChecks // {
            cached = cachedCheck;
            inherit lints;
          };

          atticPush = pkgs.writeShellApplication {
            name = "attic-push";
            runtimeInputs = [ pkgs.attic-client ];
            text = ''
              if [[ -z "''${ATTIC_TOKEN:-}" ]]; then
                echo "No ATTIC_TOKEN secret, so nothing is pushed."
                exit 0
              fi
              attic login homelab https://attic.tail174781.ts.net "$ATTIC_TOKEN"
              paths=(${pkgs.lib.escapeShellArgs (map builtins.unsafeDiscardStringContext cachedPaths)})
              skip="''${ATTIC_SKIP_PATH:-}"
              built=()
              for path in "''${paths[@]}"; do
                if [[ -n "$skip" && "$path" == "$skip" ]]; then
                  continue
                fi
                if [[ -e "$path" ]]; then
                  built+=("$path")
                fi
              done
              if (( ''${#built[@]} == 0 )); then
                echo "This job built nothing the cache keeps."
                exit 0
              fi
              printf '%s\n' "''${built[@]}"
              attic push homelab:main "''${built[@]}"
            '';
          };

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
              atticPush
              ;
            compileEnv = ''
              export TRANSCRIBE_CMAKE_ARGS="${ggmlCpu}"
              export PKG_CONFIG_PATH="${
                pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" speechLibs
              }:${libghosttyVt.dev}/lib/pkgconfig''${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
              ${macBuildEnv}
            '';
          };

          terminal-browser = import ./nix/terminal-browser.nix { inherit pkgs; };
        in
        {
          inherit
            dev
            luned
            terminal-browser
            checks
            cachedPaths
            ;
          flaky-report = flakyReport.bin;
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
      inherit ciRunners;

      devShells = nixpkgs.lib.mapAttrs (_: built: { default = built.shell; }) bySystem;

      packages = nixpkgs.lib.mapAttrs (_: built: {
        inherit (built)
          dev
          flaky-report
          luned
          terminal-browser
          ;
        default = built.luned;
      }) bySystem;

      checks = nixpkgs.lib.mapAttrs (_: built: built.checks) bySystem;

      testedTargets = nixpkgs.lib.mapAttrs (system: runner: {
        inherit runner;
        path = bySystem.${system}.checks.luned.outPath;
      }) ciRunners;

      cached = nixpkgs.lib.mapAttrs (_: built: built.cachedPaths) bySystem;
    };
}
