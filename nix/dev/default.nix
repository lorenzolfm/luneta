{
  pkgs,
  toolchain,
  buildTools,
  compileEnv,
  macBuildEnv,
  hostBuildScriptsSkipped,
  cross,
  clippyRun,
  daemonPackages,
  testRun,
  nextest,
  testTools,
  atticPush,
}:
let
  inherit (pkgs) lib;

  testKit =
    buildTools
    ++ testTools
    ++ [
      pkgs.bash
      pkgs.gawk
      pkgs.gnused
    ];

  args = {
    inherit
      pkgs
      toolchain
      buildTools
      compileEnv
      macBuildEnv
      hostBuildScriptsSkipped
      cross
      clippyRun
      daemonPackages
      testRun
      nextest
      testKit
      ;
  };

  commandFiles = lib.filterAttrs (name: kind: kind == "regular" && lib.hasSuffix ".nix" name) (
    builtins.readDir ./commands
  );

  reserved = [
    "help"
    "-h"
    "--help"
  ];

  loadCommand =
    file:
    let
      stem = lib.removeSuffix ".nix" file;
      record = import (./commands + "/${file}") args;
      checks = [
        {
          ok = record.name == stem;
          message = "nix/dev/commands/${file} sets name = ${builtins.toJSON record.name}, expected ${builtins.toJSON stem}";
        }
        {
          ok = !(builtins.elem record.name reserved);
          message = "nix/dev/commands/${file} shadows reserved dev command ${builtins.toJSON record.name}";
        }
        {
          ok = builtins.match "[a-z][a-z0-9-]*" record.name != null;
          message = "nix/dev/commands/${file} sets invalid command name ${builtins.toJSON record.name}";
        }
      ];
    in
    lib.foldr (check: next: lib.throwIfNot check.ok check.message next) record checks;

  records = map loadCommand (builtins.attrNames commandFiles);

  command =
    record:
    let
      qLabel = lib.escapeShellArg "dev-${record.name}";
    in
    pkgs.writeShellApplication {
      name = "dev-${record.name}";
      runtimeInputs = record.tools;
      inheritPath = record.inheritPath or false;
      text = ''
        enter_checkout() {
          local root config
          root="$(pwd -P)"
          while [ ! -f "$root/crates/luned/Cargo.toml" ]; do
            if [ -e "$root/.git" ] || [ "$root" = / ] || [ -z "$root" ]; then
              printf '%s: run this inside the lune checkout\n' ${qLabel} >&2
              exit 2
            fi
            root="''${root%/*}"
            root="''${root:-/}"
          done
          for config in "''${root%/}/.cargo/config.toml" "''${root%/}/.cargo/config"; do
            if [ -e "$config" ] || [ -L "$config" ]; then
              printf '%s: %s\n' ${qLabel} "$config exists, which flake.nix's cargoFiles ignores while dev and plain cargo read; remove it or wire it into flake.nix and ci.yml" >&2
              exit 2
            fi
          done
          export DEV_HERE="$PWD"
          export DEV_PATH="''${DEV_PATH:-$PATH}"
          cd "$root"
        }
        enter_checkout
        export PATH="$PATH:''${CARGO_HOME:-''${HOME:-}/.cargo}/bin"
        ${record.text}
      '';
    };

  commands = lib.mapAttrs' (
    file: _: lib.nameValuePair (lib.removeSuffix ".nix" file) (command (loadCommand file))
  ) commandFiles;

  signature = record: "dev ${record.name} ${record.usage}";
  width = lib.foldl' lib.max 0 (map (record: lib.stringLength (signature record)) records);

  helpLines = lib.concatMapStringsSep "\n" (
    record:
    "printf '  %s%-${toString width}s%s  %s\\n' \"$cyan\" ${lib.escapeShellArg (signature record)} \"$reset\" ${lib.escapeShellArg record.help}"
  ) records;

  dispatch = lib.concatMapStringsSep "\n" (record: ''
    ${record.name})
      export DEV_PATH="$PATH"
      exec ${lib.getExe commands.${record.name}} "$@"
      ;;
  '') records;
in
pkgs.writeShellApplication {
  name = "dev";
  meta.description = "lune build recipes on the flake-pinned toolchain";
  passthru.commands = {
    inherit (commands) lint deny;
    attic-push = atticPush;
    ios-build = command (import ./commands/ios.nix args).build;
  };
  text = ''
    paint() {
      if [ -t "$1" ] && [ -z "''${NO_COLOR:-}" ]; then
        bold=$'\033[1m'
        cyan=$'\033[36m'
        red=$'\033[1;31m'
        reset=$'\033[0m'
      else
        bold=""
        cyan=""
        red=""
        reset=""
      fi
    }

    usage() {
      paint "$1"
      printf '%s%s%s\n\n' "$bold" 'dev — lune build recipes (pinned nix tooling)' "$reset"
      ${helpLines}
    }

    error() {
      paint 2
      printf '%sdev:%s %s\n' "$red" "$reset" "$1" >&2
    }

    cmd="''${1:-help}"
    shift || true

    case "$cmd" in
      help | -h | --help)
        usage 1
        ;;
      ${dispatch}
      *)
        error "unknown command $cmd"
        printf '\n' >&2
        usage 2 >&2
        exit 2
        ;;
    esac
  '';
}
