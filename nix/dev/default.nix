{
  pkgs,
  toolchain,
  buildTools,
  compileEnv,
  macBuildEnv,
  clippyRun,
  testRun,
  nextest,
  testTools,
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
      clippyRun
      testRun
      nextest
      testKit
      ;
  };

  records = map (file: import (./commands + "/${file}") args) (
    builtins.attrNames (builtins.readDir ./commands)
  );

  enterCheckout =
    {
      label ? null,
      fail ? null,
    }:
    let
      report =
        msg:
        if fail != null then
          ''${fail} "${msg}"''
        else
          ''printf '%s: %s\n' ${lib.escapeShellArg label} "${msg}" >&2'';
    in
    ''
      root="$PWD"
      while [ ! -f "$root/crates/luned/Cargo.toml" ]; do
        if [ "$root" = / ] || [ -z "$root" ]; then
          ${report "run this inside the lune checkout"}
          exit 2
        fi
        root="''${root%/*}"
        root="''${root:-/}"
      done
      for config in "''${root%/}/.cargo/config.toml" "''${root%/}/.cargo/config"; do
        if [ -e "$config" ] || [ -L "$config" ]; then
          ${report "$config exists, which flake.nix's cargoFiles ignores while dev and plain cargo read; remove it or wire it into flake.nix and ci.yml"}
          exit 2
        fi
      done
      export DEV_HERE="$PWD"
      export DEV_PATH="$PATH"
      cd "$root"
    '';

  command =
    record:
    pkgs.writeShellApplication {
      name = "dev-${record.name}";
      runtimeInputs = record.tools;
      inheritPath = record.inheritPath or false;
      text = ''
        export PATH="$PATH:''${CARGO_HOME:-''${HOME:-}/.cargo}/bin"
        ${record.text}
      '';
    };

  standalone =
    record:
    command (
      record
      // {
        text = ''
          ${enterCheckout { label = "dev-${record.name}"; }}
          ${record.text}
        '';
      }
    );

  signature = record: "dev ${record.name} ${record.usage}";
  width = lib.foldl' lib.max 0 (map (record: lib.stringLength (signature record)) records);

  helpLines = lib.concatMapStringsSep "\n" (
    record:
    "printf '  %s%-${toString width}s%s  %s\\n' \"$cyan\" ${lib.escapeShellArg (signature record)} \"$reset\" ${lib.escapeShellArg record.help}"
  ) records;

  dispatch = lib.concatMapStringsSep "\n" (record: ''
    ${record.name})
      enter_checkout
      exec ${lib.getExe (command record)} "$@"
      ;;
  '') records;
in
pkgs.writeShellApplication {
  name = "dev";
  meta.description = "lune build recipes on the flake-pinned toolchain";
  passthru.commands = {
    lint = standalone (import ./commands/lint.nix args);
    clippy-cross = standalone (import ./commands/clippy.nix args).cross;
    ios-build = standalone (import ./commands/ios.nix args).build;
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

    enter_checkout() {
      ${enterCheckout { fail = "error"; }}
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
