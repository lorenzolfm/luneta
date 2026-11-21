{ pkgs }:
let
  inherit (pkgs) lib;
  stops = ";|&<>";
  word = ''(?:[^\s${stops}\\]|\\[^n\s])+'';
  call =
    gap:
    ''(^|[^\w-])(?i:cargo)(?:${word})?${gap}(?:${word}${gap})*?nextest${gap}(?:${word}${gap})*?(run|r)([\s${stops})"'}\\]|$)'';
  joined = ''(?:[ \t]|\\\n)+'';
  folded = ''(?:[ \t]+(?:\n[ \t]*)?|\n[ \t]*)'';
  wrapper = "luneta-nextest (nix/nextest.nix): it runs nextest on luneta's packages and fails when the tests leave a luneta-mux from target/ running";
  inString = {
    any = [
      { kind = "string_expression"; }
      { kind = "indented_string_expression"; }
    ];
    regex = call joined;
  };
  literal = {
    field = "value";
    regex = ''^\|'';
  };
  rules = [
    {
      id = "cargo-nextest-through-luneta-nextest";
      language = "nix";
      ignores = [ "nix/nextest.nix" ];
      message = "Run the tests through ${wrapper}, which a raw nextest run, or crane's cargoNextest, lets pass.";
      rule.any = [
        {
          kind = "identifier";
          regex = "^cargoNextest$";
        }
        (
          inString
          // {
            not.has = inString // {
              stopBy = "end";
            };
          }
        )
      ];
    }
    {
      id = "cargo-nextest-through-luneta-nextest-in-workflows";
      language = "yaml";
      files = [ ".github/workflows/**" ];
      message = "Run the tests through ${wrapper}, which a raw nextest run lets pass. luneta-nextest is on the devshell's PATH.";
      rule = {
        kind = "block_mapping_pair";
        has = {
          field = "key";
          regex = ''^["']?run["']?$'';
        };
        any = [
          {
            has = literal;
            regex = call joined;
          }
          {
            not.has = literal;
            regex = call folded;
          }
        ];
      };
    }
  ];
in
pkgs.writeTextDir "nextest.yml" (lib.concatMapStringsSep "\n---\n" builtins.toJSON rules)
