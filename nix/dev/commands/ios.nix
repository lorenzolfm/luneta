{
  pkgs,
  toolchain,
  macBuildEnv,
  ...
}:
let
  inherit (pkgs) lib;
  onMac = pkgs.stdenv.hostPlatform.isDarwin;
  phone = "aarch64-apple-ios";
  simulator = "aarch64-apple-ios-sim";
  toolchainFor =
    targets:
    lib.throwIfNot (targets != [ ]) "toolchainFor requires at least one Rust target" (
      toolchain.override { inherit targets; }
    );
  xcode = import ../../ios.nix { inherit pkgs; };
  swift = import ../../swift.nix { inherit pkgs; };
  noXcode = label: ''
    echo "${label}: the app is built with Xcode, which only a Mac has" >&2
    exit 2
  '';
  buildApp =
    {
      label,
      mode,
      target,
      profile,
      folder,
      configuration,
      sdk,
      destination,
      xcodebuildArgs,
    }:
    lib.throwIfNot
      (lib.elem mode [
        "build"
        "run"
        "phone"
      ])
      "buildApp: unsupported mode ${mode}"
      (
        lib.throwIfNot (target != "" && profile != "" && folder != "" && configuration != "" && sdk != "")
          "buildApp: build coordinates must be non-empty"
          ''
            require-xcode
            ${macBuildEnv}
            cargo build --locked -p lune-phone --target ${lib.escapeShellArg target} --profile ${lib.escapeShellArg profile}
            cargo build --locked -p lune-phone-bindgen
            built="$(cargo metadata --locked --no-deps --format-version 1 | jq -r .target_directory)"
            if [ -z "$built" ] || [ ! -d "$built" ]; then
              echo "${label}: cargo metadata returned no target directory" >&2
              exit 1
            fi
            library="$built/${target}/${folder}/liblune_phone.a"
            bindgen="$built/debug/uniffi-bindgen-swift"
            if [ ! -f "$library" ]; then
              echo "${label}: missing built library $library" >&2
              exit 1
            fi
            if [ ! -x "$bindgen" ]; then
              echo "${label}: missing bindgen executable $bindgen" >&2
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

            xcodebuild_args=(${xcodebuildArgs})
            xcodebuild-logged ${lib.escapeShellArg mode} build -project ios/Lune.xcodeproj -scheme Lune \
              -configuration ${lib.escapeShellArg configuration} -sdk ${lib.escapeShellArg sdk} \
              -destination ${destination} \
              -derivedDataPath "$out/derived" "''${xcodebuild_args[@]}"
            app="$out/derived/Build/Products/${configuration}-${sdk}/Lune.app"
            if [ ! -f "$app/Info.plist" ]; then
              echo "${label}: missing built app manifest $app/Info.plist" >&2
              exit 1
            fi
            bundle="$(plutil -extract CFBundleIdentifier raw -o - "$app/Info.plist")"
            if [ -z "$bundle" ]; then
              echo "${label}: empty CFBundleIdentifier in $app/Info.plist" >&2
              exit 1
            fi
          ''
      );
  onXcode = ''
    case "$how" in
      clean)
        rm -rf ios/build ios/Lune.xcodeproj
        checkout-simulator delete
        ;;
      build)
        ${buildApp {
          label = "dev ios";
          mode = "build";
          target = simulator;
          profile = "dev";
          folder = "debug";
          configuration = "Debug";
          sdk = "iphonesimulator";
          destination = ''"generic/platform=iOS Simulator"'';
          xcodebuildArgs = "ARCHS=arm64";
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
          target = simulator;
          profile = "dev";
          folder = "debug";
          configuration = "Debug";
          sdk = "iphonesimulator";
          destination = ''"id=$udid"'';
          xcodebuildArgs = "ARCHS=arm64";
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
        ${buildApp {
          label = "dev ios";
          mode = "phone";
          target = phone;
          profile = "release";
          folder = "release";
          configuration = "Release";
          sdk = "iphoneos";
          destination = ''"generic/platform=iOS"'';
          xcodebuildArgs = ''-allowProvisioningUpdates "DEVELOPMENT_TEAM=$LUNE_TEAM"'';
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
      (toolchainFor [
        phone
        simulator
      ])
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
    tools = lib.optionals onMac ([ (toolchainFor [ simulator ]) ] ++ buildKit);
    text = ''
      if [ "$#" -ne 0 ]; then
        echo "usage: dev-ios-build" >&2
        exit 2
      fi
      ${
        if onMac then
          ''
            if [ ! -f Cargo.toml ]; then
              echo "dev-ios-build: missing Cargo.toml; run from the lune checkout root" >&2
              exit 2
            fi
            if [ ! -f ios/project.yml ]; then
              echo "dev-ios-build: missing ios/project.yml; run from the lune checkout root" >&2
              exit 2
            fi
            for config in .cargo/config.toml .cargo/config; do
              if [ -e "$config" ] || [ -L "$config" ]; then
                echo "dev-ios-build: $config exists, which flake.nix's cargoFiles ignores while dev and plain cargo read; remove it or wire it into flake.nix and ci.yml" >&2
                exit 2
              fi
            done
            ${buildApp {
              label = "dev-ios-build";
              mode = "build";
              target = simulator;
              profile = "dev";
              folder = "debug";
              configuration = "Debug";
              sdk = "iphonesimulator";
              destination = ''"generic/platform=iOS Simulator"'';
              xcodebuildArgs = "ARCHS=arm64";
            }}
            echo "dev-ios-build: built $bundle in $app" >&2
          ''
        else
          noXcode "dev-ios-build"
      }
    '';
  };
}
