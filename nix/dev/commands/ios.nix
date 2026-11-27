{
  pkgs,
  toolchain,
  macBuildEnv,
  ...
}:
let
  inherit (pkgs) lib;
  onMac = pkgs.stdenv.hostPlatform.isDarwin;
  profileFolders = {
    dev = "debug";
    release = "release";
  };
  mkPlatform =
    {
      target,
      profile,
      configuration,
      sdk,
      xcodebuildArgs,
    }:
    {
      inherit
        target
        profile
        configuration
        sdk
        xcodebuildArgs
        ;
      folder = profileFolders.${profile};
    };
  platforms = {
    simulator = mkPlatform {
      target = "aarch64-apple-ios-sim";
      profile = "dev";
      configuration = "Debug";
      sdk = "iphonesimulator";
      xcodebuildArgs = [ (lib.escapeShellArg "ARCHS=arm64") ];
    };
    phone = mkPlatform {
      target = "aarch64-apple-ios";
      profile = "release";
      configuration = "Release";
      sdk = "iphoneos";
      xcodebuildArgs = [
        "-allowProvisioningUpdates"
        ''"DEVELOPMENT_TEAM=$LUNE_TEAM"''
      ];
    };
  };
  builds = {
    build = {
      platform = platforms.simulator;
      destination = lib.escapeShellArg "generic/platform=iOS Simulator";
    };
    run = {
      platform = platforms.simulator;
      destination = ''"id=$udid"'';
    };
    phone = {
      platform = platforms.phone;
      destination = lib.escapeShellArg "generic/platform=iOS";
    };
  };
  toolchainFor = targets: toolchain.override { inherit targets; };
  xcode = import ../../ios.nix { inherit pkgs; };
  swift = import ../../swift.nix { inherit pkgs; };
  noXcode = label: ''
    printf '%s: the app is built with Xcode, which only a Mac has\n' ${lib.escapeShellArg label} >&2
    exit 2
  '';
  buildApp =
    { label, mode }:
    let
      spec = builds.${mode};
      p = spec.platform;
      qLabel = lib.escapeShellArg label;
    in
    ''
      if [ ! -f ios/project.yml ]; then
        printf '%s: missing ios/project.yml; run from the lune checkout root\n' ${qLabel} >&2
        exit 2
      fi
      ${macBuildEnv}
      cargo build --locked -p lune-phone --target ${lib.escapeShellArg p.target} --profile ${lib.escapeShellArg p.profile}
      cargo clippy --locked -p lune-phone-bindgen --all-targets -- -D warnings
      cargo build --locked -p lune-phone-bindgen
      built="$(cargo metadata --locked --no-deps --format-version 1 | jq -er '.target_directory | strings')"
      if [ -z "$built" ] || [ ! -d "$built" ]; then
        printf '%s: cargo metadata returned no target directory\n' ${qLabel} >&2
        exit 1
      fi
      library="$built/${p.target}/${p.folder}/liblune_phone.a"
      bindgen="$built/debug/uniffi-bindgen-swift"
      if [ ! -f "$library" ]; then
        printf '%s: missing built library %s\n' ${qLabel} "$library" >&2
        exit 1
      fi
      if [ ! -x "$bindgen" ]; then
        printf '%s: missing bindgen executable %s\n' ${qLabel} "$bindgen" >&2
        exit 1
      fi

      out=ios/build
      rm -rf "$out/bindings" "$out/headers" "$out/LunePhone.xcframework"
      mkdir -p "$out/bindings" "$out/headers"
      "$bindgen" --swift-sources "$library" "$out/bindings"
      "$bindgen" --headers --modulemap --module-name lune_phoneFFI \
        --modulemap-filename module.modulemap "$library" "$out/headers"
      xcodebuild -create-xcframework -library "$library" -headers "$out/headers" \
        -output "$out/LunePhone.xcframework" >/dev/null

      xcodegen generate --quiet --spec ios/project.yml

      xcodebuild_args=(${lib.concatStringsSep " " p.xcodebuildArgs})
      xcodebuild-logged ${lib.escapeShellArg mode} build -project ios/Lune.xcodeproj -scheme Lune \
        -configuration ${lib.escapeShellArg p.configuration} -sdk ${lib.escapeShellArg p.sdk} \
        -destination ${spec.destination} \
        -derivedDataPath "$out/derived" "''${xcodebuild_args[@]}"
      app="$out/derived/Build/Products/${p.configuration}-${p.sdk}/Lune.app"
      if [ ! -f "$app/Info.plist" ]; then
        printf '%s: missing built app manifest %s/Info.plist\n' ${qLabel} "$app" >&2
        exit 1
      fi
      bundle="$(plutil -extract CFBundleIdentifier raw -o - "$app/Info.plist")"
      if [ -z "$bundle" ]; then
        printf '%s: empty CFBundleIdentifier in %s/Info.plist\n' ${qLabel} "$app" >&2
        exit 1
      fi
    '';
  onXcode = ''
    case "$how" in
      clean)
        rm -rf ios/build ios/Lune.xcodeproj
        checkout-simulator delete
        ;;
      build)
        require-xcode
        ${buildApp {
          label = "dev ios";
          mode = "build";
        }}
        echo "dev ios: built $bundle in $app" >&2
        ;;
      run)
        require-xcode
        udid="$(checkout-simulator ensure)"
        if [ -z "$udid" ]; then
          echo "dev ios: checkout-simulator ensure returned no udid" >&2
          exit 1
        fi
        ${buildApp {
          label = "dev ios";
          mode = "run";
        }}
        xcrun simctl bootstatus "$udid" -b >/dev/null
        xcrun simctl install "$udid" "$app"
        echo "dev ios: $bundle runs on $(checkout-simulator name), which DeviceHub shows" >&2
        exec xcrun simctl launch --console-pty --terminate-running-process "$udid" "$bundle"
        ;;
      phone)
        if [ -z "''${LUNE_TEAM:-}" ]; then
          echo "dev ios: LUNE_TEAM is unset: sign into Xcode (Settings > Accounts) and set it to your team's ID" >&2
          exit 2
        fi
        require-xcode
        ${buildApp {
          label = "dev ios";
          mode = "phone";
        }}
        listed="$(mktemp)"
        trap 'rm -f "$listed"' EXIT
        xcrun devicectl list devices --quiet --json-output "$listed" >/dev/null
        mapfile -t phones < <(jq -r '.result.devices[]
          | select(.hardwareProperties.reality == "physical" and .hardwareProperties.platform == "iOS")
          | .hardwareProperties.udid' "$listed")
        device="''${LUNE_PHONE:-}"
        if [ -z "$device" ]; then
          case "''${#phones[@]}" in
            1) device="''${phones[0]}" ;;
            0)
              echo "dev ios: no iPhone is paired with this Mac" >&2
              exit 2
              ;;
            *)
              echo "dev ios: more than one iPhone is paired, so set LUNE_PHONE to one of: ''${phones[*]}" >&2
              exit 2
              ;;
          esac
        fi
        xcrun devicectl device install app --device "$device" "$app"
        xcrun devicectl device process launch --device "$device" "$bundle"
        ;;
      *)
        echo "dev ios: unexpected mode $how" >&2
        exit 1
        ;;
    esac
  '';
  buildKit = lib.optionals onMac (
    [
      pkgs.coreutils
      pkgs.jq
      pkgs.protobuf
      pkgs.stdenv.cc
      pkgs.stdenv.cc.bintools.bintools
    ]
    ++ xcode.buildTools
  );
in
{
  name = "ios";
  usage = "[build|run|phone|clean|fmt]";
  help = "the iPhone app, on a Mac with Xcode ${xcode.xcodeVersion}: build for the simulator (the default), run it on this checkout's simulator, phone to install it on the paired iPhone (LUNE_TEAM names the signing team, LUNE_PHONE the device when more than one is paired), clean to drop ios/build and the simulator, fmt to format the Swift (on Linux too)";
  tools = [
    pkgs.git
    pkgs.findutils
    swift.format
  ]
  ++ lib.optionals onMac (
    [
      (toolchainFor (map (p: p.target) (builtins.attrValues platforms)))
    ]
    ++ buildKit
    ++ xcode.runTools
  );
  text = ''
    case "$#:$*" in
      0: | 1:build) how=build ;;
      1:run) how=run ;;
      1:phone) how=phone ;;
      1:clean) how=clean ;;
      1:fmt) how=fmt ;;
      *)
        echo "usage: dev ios [build|run|phone|clean|fmt]" >&2
        exit 2
        ;;
    esac

    if [ "$how" = fmt ]; then
      git ls-files -z --cached --others --exclude-standard '*.swift' | xargs -0 -r swift-format-lune
      exit 0
    fi

    ${if onMac then onXcode else noXcode "dev ios"}
  '';
  build = {
    name = "ios-build";
    tools = lib.optionals onMac ([ (toolchainFor [ platforms.simulator.target ]) ] ++ buildKit);
    text = ''
      if [ "$#" -ne 0 ]; then
        echo "usage: dev-ios-build" >&2
        exit 2
      fi
      ${
        if onMac then
          ''
            require-xcode
            ${buildApp {
              label = "dev-ios-build";
              mode = "build";
            }}
            echo "dev-ios-build: built $bundle in $app" >&2
          ''
        else
          noXcode "dev-ios-build"
      }
    '';
  };
}
