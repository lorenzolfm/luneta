{ pkgs }:
let
  xcodeVersion = "27.2";
  deviceType = "com.apple.CoreSimulator.SimDeviceType.iPhone-17";

  system =
    name:
    pkgs.writeShellScriptBin name ''
      kept=(HOME="$HOME" USER="''${USER:-}" TMPDIR="''${TMPDIR:-/tmp}" PATH=/usr/bin:/bin:/usr/sbin:/sbin)
      case "''${DEVELOPER_DIR:-}" in
        "" | /nix/store/*) ;;
        *) kept+=(DEVELOPER_DIR="$DEVELOPER_DIR") ;;
      esac
      exec /usr/bin/env -i "''${kept[@]}" /usr/bin/${name} "$@"
    '';

  xcodebuild = system "xcodebuild";
  xcrun = system "xcrun";
  plutil = system "plutil";

  requireXcode = pkgs.writeShellApplication {
    name = "require-xcode";
    runtimeInputs = [
      pkgs.gnused
      xcodebuild
    ];
    text = ''
      errors="$(mktemp)"
      trap 'rm -f "$errors"' EXIT
      if ! output="$(xcodebuild -version 2>"$errors")"; then
        cat "$errors" >&2
        echo "dev ios: install Xcode ${xcodeVersion} and select it with xcode-select --switch, or name it in DEVELOPER_DIR" >&2
        exit 2
      fi
      version="$(printf '%s\n' "$output" | sed -n '/^Xcode /{s///p;q}')"
      if [ "$version" != ${xcodeVersion} ]; then
        echo "dev ios: this is Xcode ''${version:-of no version}, and lune's app is built with ${xcodeVersion}: select that one with xcode-select --switch, or name it in DEVELOPER_DIR" >&2
        exit 2
      fi
    '';
  };

  checkoutSimulator = pkgs.writeShellApplication {
    name = "checkout-simulator";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.jq
      xcrun
    ];
    text = ''
      name="lune $(basename "$PWD") $(printf '%s' "$PWD" | cksum | cut -d ' ' -f 1)"

      runtime() {
        local sdk id
        sdk="$(xcrun --sdk iphonesimulator --show-sdk-version)"
        id="$(xcrun simctl list runtimes --json | jq --raw-output \
          --arg version "$sdk" --arg type ${deviceType} '
            [.runtimes[]
              | select(.platform == "iOS" and .version == $version and .isAvailable)
              | select(any(.supportedDeviceTypes[]; .identifier == $type))
            ][0].identifier // empty')"
        if [ -z "$id" ]; then
          echo "dev ios: no iOS $sdk simulator runtime runs ${deviceType}: install one with xcodebuild -downloadPlatform iOS" >&2
          exit 2
        fi
        printf '%s\n' "$id"
      }

      found() {
        xcrun simctl list devices --json | jq --raw-output --arg runtime "$1" --arg name "$name" '
          [.devices[$runtime][]? | select(.name == $name and .isAvailable)][0].udid // empty'
      }

      case "$#:''${1:-}" in
        1:name)
          printf '%s\n' "$name"
          ;;
        1:ensure)
          id="$(runtime)"
          udid="$(found "$id")"
          if [ -z "$udid" ]; then
            udid="$(xcrun simctl create "$name" ${deviceType} "$id")"
            echo "dev ios: made the simulator $name ($udid)" >&2
          fi
          printf '%s\n' "$udid"
          ;;
        1:delete)
          xcrun simctl list devices --json |
            jq --raw-output --arg name "$name" \
              '.devices[][] | select(.name == $name) | "\(.udid) \(.state)"' |
            while read -r udid state; do
              if [ "$state" != Shutdown ]; then
                xcrun simctl shutdown "$udid"
              fi
              xcrun simctl delete "$udid"
              echo "dev ios: deleted the simulator $name ($udid)" >&2
            done
          ;;
        *)
          echo "usage: checkout-simulator name | ensure | delete" >&2
          exit 2
          ;;
      esac
    '';
  };

  xcodebuildLogged = pkgs.writeShellApplication {
    name = "xcodebuild-logged";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.xcbeautify
      xcodebuild
    ];
    text = ''
      log="ios/build/logs/$1.log"
      shift
      mkdir -p ios/build/logs
      flags=(--disable-logging)
      if [ "''${GITHUB_ACTIONS:-}" = true ]; then
        flags+=(--renderer github-actions)
      elif [ ! -t 1 ] || [ -n "''${NO_COLOR:-}" ]; then
        flags+=(--disable-colored-output)
      fi
      status=0
      xcodebuild "$@" 2>&1 | tee "$log" | xcbeautify "''${flags[@]}" || status=$?
      if [ "$status" -ne 0 ]; then
        echo "dev ios: xcodebuild's whole output is in $log" >&2
      fi
      exit "$status"
    '';
  };

  buildTools = [
    pkgs.xcodegen
    plutil
    requireXcode
    xcodebuild
    xcodebuildLogged
  ];

  runTools = [
    checkoutSimulator
    xcrun
  ];
in
{
  inherit
    xcodeVersion
    buildTools
    runTools
    ;

  tools = buildTools ++ runTools;
}
