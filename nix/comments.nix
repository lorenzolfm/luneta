{ pkgs }:
let
  comments = ../.config/ast-grep/comments;
  grammar = language: "${pkgs.tree-sitter-grammars."tree-sitter-${language}"}/parser";
  extensions = {
    fish = [ "fish" ];
    proto = [ "proto" ];
    toml = [ "toml" ];
    vhs = [ "tape" ];
  };
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
in
pkgs.writeText "comments.yml" (
  builtins.toJSON {
    ruleDirs = [ "${comments}/rules" ];
    testConfigs = [ { testDir = "${comments}/rule-tests"; } ];
    customLanguages = builtins.mapAttrs (language: extensions: {
      libraryPath = grammar language;
      inherit extensions;
    }) extensions;
    languageGlobs.bash = [
      ".envrc"
      ".gitignore"
    ];
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
)
