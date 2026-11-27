{ pkgs }:
let
  inherit (pkgs) lib;
  comments = ../.config/ast-grep/comments;
  waits = ../.config/ast-grep/waits;
  calls = ../.config/ast-grep/calls;
  nextestRules = import ./nextest-rules.nix { inherit pkgs; };
  binTestRules = pkgs.writeTextDir "bin-tests.yml" (import ./bin-test-rules.nix { root = ../.; });
  grammar = language: "${pkgs.tree-sitter-grammars."tree-sitter-${language}"}/parser";
  extensions = {
    fish = [ "fish" ];
    proto = [ "proto" ];
    toml = [ "toml" ];
    vhs = [ "tape" ];
  };
  bashNames = [
    ".envrc"
    ".gitignore"
  ];
  skipped = [
    "LICENSE"
    "*.lock"
    "*.md"
    "*.png"
  ];
  suppression = "ast-grep" + "-ignore";
  runBlock = {
    kind = "block_node";
    inside = {
      kind = "block_mapping_pair";
      has = {
        field = "key";
        regex = "^run$";
      };
    };
  };
  configFolder = pkgs.writeTextDir "sgconfig.yml" (
    builtins.toJSON {
      ruleDirs = [
        "${comments}/rules"
        "${waits}/rules"
        "${calls}/rules"
        "${nextestRules}"
        "${binTestRules}"
      ];
      utilDirs = [ "${waits}/utils" ];
      testConfigs = [
        { testDir = "${comments}/rule-tests"; }
        { testDir = "${waits}/rule-tests"; }
        { testDir = "${calls}/rule-tests"; }
      ];
      customLanguages = builtins.mapAttrs (language: extensions: {
        libraryPath = grammar language;
        inherit extensions;
      }) extensions;
      languageGlobs.bash = bashNames;
      languageInjections = [
        {
          hostLanguage = "nix";
          rule = {
            kind = "indented_string_expression";
            pattern = "$CONTENT";
          };
          injected = "bash";
        }
        {
          hostLanguage = "yaml";
          rule = {
            kind = "block_scalar";
            pattern = "$CONTENT";
            inside = runBlock;
          };
          injected = "bash";
        }
      ];
    }
  );
  config = "${configFolder}/sgconfig.yml";
  configDir = dirOf config;
in
pkgs.writeShellApplication {
  name = "lune-comments";
  runtimeInputs = [
    pkgs.ast-grep
    pkgs.coreutils
    pkgs.findutils
    pkgs.gnugrep
    pkgs.jq
  ];
  text = ''
    tmp_config=""
    inspect=""
    trap '[ -z "$inspect" ] || rm -f "$inspect"; [ -z "$tmp_config" ] || rm -rf "$tmp_config"' EXIT
    config=${config}
    config_dir=${configDir}
    if (( $# == 2 )) && [[ $1 == --bin-rules ]]; then
      tmp_config="$(mktemp -d)"
      jq --arg old ${lib.escapeShellArg "${binTestRules}"} --arg new "$2" \
        '.ruleDirs |= map(if . == $old then $new else . end)' ${config} >"$tmp_config/sgconfig.yml"
      config="$tmp_config/sgconfig.yml"
      config_dir="$tmp_config"
    elif (( $# != 0 )); then
      echo "usage: lune-comments [--bin-rules <dir>]" >&2
      exit 2
    fi
    case "$PWD/" in
      "$config_dir"/*)
        echo "lune-comments: the ast-grep config's folder $config_dir holds $PWD, so its rules' files: and ignores: globs would see paths under that folder, not paths from $PWD" >&2
        exit 1
        ;;
    esac
    status=0
    found=()
    while IFS= read -r -d "" path; do
      if [[ ! -e $path ]]; then
        continue
      fi
      case ''${path##*/} in
        ${lib.concatStringsSep " | " skipped}) ;;
        *) found+=("''${path#./}") ;;
      esac
    done
    if (( ''${#found[@]} == 0 )); then
      echo "lune-comments: no file to scan was listed on stdin" >&2
      exit 1
    fi
    ast-grep test --config "$config" || status=1
    inspect="$(mktemp)"
    printf '%s\0' "''${found[@]}" |
      xargs -0 ast-grep scan --error --inspect entity --config "$config" 2>"$inspect" || status=1
    ruled_file='^sg: entity[|]file[|](.*): language=[^,]*,appliedRuleCount=[1-9][0-9]*$'
    declare -A ruled=()
    while IFS= read -r line; do
      if [[ $line =~ $ruled_file ]]; then
        ruled[''${BASH_REMATCH[1]}]=1
      elif [[ $line != "sg: "* ]]; then
        echo "$line" >&2
      fi
    done <"$inspect"
    for path in "''${found[@]}"; do
      case $path in
        *.sh | *.bash)
          echo "lune-comments: $path is a shell script file; it lives inline in a nix file, as writeShellApplication's text, so nix is the only way to run it" >&2
          status=1
          continue
          ;;
      esac
      if [[ -z ''${ruled[$path]-} ]]; then
        echo "lune-comments: no rule says what a comment is in $path" >&2
        status=1
      fi
    done
    if grep -lF -- ${lib.escapeShellArg suppression} "''${found[@]}" >&2; then
      echo "lune-comments: the files above turn a rule off with ${suppression}" >&2
      status=1
    elif (( $? != 1 )); then
      status=1
    fi
    exit "$status"
  '';
}
