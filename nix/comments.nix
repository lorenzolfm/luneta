{ pkgs }:
let
  inherit (pkgs) lib;
  comments = ../.config/ast-grep/comments;
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
  scanned = bashNames ++ map (extension: "*.${extension}") (
    [
      "nix"
      "rs"
      "ts"
      "yml"
    ]
    ++ lib.concatLists (builtins.attrValues extensions)
  );
  skipped = [
    "LICENSE"
    "*.lock"
    "*.md"
    "*.png"
  ];
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
  config = pkgs.writeText "comments.yml" (
    builtins.toJSON {
      ruleDirs = [ "${comments}/rules" ];
      testConfigs = [ { testDir = "${comments}/rule-tests"; } ];
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
  patterns = lib.concatStringsSep " | ";
in
pkgs.writeShellApplication {
  name = "luneta-comments";
  runtimeInputs = [
    pkgs.ast-grep
    pkgs.findutils
  ];
  text = ''
    status=0
    found=()
    while IFS= read -r -d "" path; do
      if [[ ! -e $path ]]; then
        continue
      fi
      case ''${path##*/} in
        ${patterns scanned})
          found+=("$path")
          ;;
        ${patterns skipped}) ;;
        *)
          echo "luneta-comments: no rule says what a comment is in $path" >&2
          status=1
          ;;
      esac
    done
    if (( ''${#found[@]} == 0 )); then
      echo "luneta-comments: no file to scan was listed on stdin" >&2
      exit 1
    fi
    ast-grep test --config ${config} || status=1
    printf '%s\0' "''${found[@]}" | xargs -0 ast-grep scan --config ${config} || status=1
    exit "$status"
  '';
}
