{
  pkgs,
  toolchain,
  buildTools,
  compileEnv,
  ...
}:
let
  inherit (pkgs) lib;
  onMac = pkgs.stdenv.hostPlatform.isDarwin;
  phone = "aarch64-apple-ios";
  simulator = "aarch64-apple-ios-sim";
  iosToolchain = toolchain.override {
    targets = [
      phone
      simulator
    ];
  };
  xcode = import ../../ios.nix { inherit pkgs; };
  swift = import ../../swift.nix { inherit pkgs; };
  noXcode = ''
    echo "dev ios: the app is built with Xcode, which only a Mac has" >&2
    exit 2
  '';
  onXcode = ''
    if [ "$how" = clean ]; then
      rm -rf ios/build ios/Lune.xcodeproj
      checkout-simulator delete
      exit 0
    fi

    require-xcode

    if [ "$how" = phone ]; then
      [ -n "''${LUNE_TEAM:-}" ] || {
        echo "dev ios: LUNE_TEAM is unset: sign into Xcode (Settings > Accounts) and set it to your team's ID" >&2
        exit 2
      }
      target=${phone}
      profile=release
      folder=release
      configuration=Release
      sdk=iphoneos
      destination="generic/platform=iOS"
      signing=(-allowProvisioningUpdates "DEVELOPMENT_TEAM=$LUNE_TEAM")
    else
      target=${simulator}
      profile=dev
      folder=debug
      configuration=Debug
      sdk=iphonesimulator
      udid="$(checkout-simulator ensure)"
      destination="id=$udid"
      signing=()
    fi

    ${compileEnv}
    cargo build --locked -p lune-phone --target "$target" --profile "$profile"
    cargo clippy --locked -p lune-phone-bindgen --all-targets -- -D warnings
    cargo build --locked -p lune-phone-bindgen
    built="$(cargo metadata --locked --no-deps --format-version 1 | jq -r .target_directory)"
    library="$built/$target/$folder/liblune_phone.a"
    bindgen="$built/debug/uniffi-bindgen-swift"

    out=ios/build
    rm -rf "$out/bindings" "$out/headers" "$out/LunePhone.xcframework"
    mkdir -p "$out/bindings" "$out/headers"
    "$bindgen" --swift-sources "$library" "$out/bindings"
    "$bindgen" --headers --modulemap --module-name lune_phoneFFI \
      --modulemap-filename module.modulemap "$library" "$out/headers"
    xcodebuild -create-xcframework -library "$library" -headers "$out/headers" \
      -output "$out/LunePhone.xcframework" >/dev/null

    xcodegen generate --quiet --spec ios/project.yml

    xcodebuild-logged "$how" build -project ios/Lune.xcodeproj -scheme Lune \
      -configuration "$configuration" -sdk "$sdk" -destination "$destination" \
      -derivedDataPath "$out/derived" "''${signing[@]}"
    app="$out/derived/Build/Products/$configuration-$sdk/Lune.app"
    bundle="$(plutil -extract CFBundleIdentifier raw -o - "$app/Info.plist")"

    case "$how" in
      build)
        echo "dev ios: built $bundle in $app" >&2
        ;;
      run)
        xcrun simctl bootstatus "$udid" -b >/dev/null
        xcrun simctl install "$udid" "$app"
        echo "dev ios: $bundle runs on $(checkout-simulator name), which DeviceHub shows" >&2
        exec xcrun simctl launch --console-pty --terminate-running-process "$udid" "$bundle"
        ;;
      phone)
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
    esac
  '';
in
{
  name = "ios";
  usage = "[build|run|phone|clean|fmt]";
  help = "the iPhone app, on a Mac with Xcode ${xcode.xcodeVersion}: build for this checkout's simulator (the default), run it there, phone to install it on the paired iPhone (LUNE_TEAM names the signing team, LUNE_PHONE the device when more than one is paired), clean to drop ios/build and the simulator, fmt to format the Swift (on Linux too)";
  tools = [
    iosToolchain
    pkgs.coreutils
    pkgs.jq
    pkgs.git
    pkgs.findutils
    swift.format
  ]
  ++ buildTools
  ++ lib.optionals onMac xcode.tools;
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

    ${if onMac then onXcode else noXcode}
  '';
}
