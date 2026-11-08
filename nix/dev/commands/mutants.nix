{
  pkgs,
  buildTools,
  testTools,
  compileEnv,
  ...
}:
{
  name = "mutants";
  usage = "[args...]";
  help = "cargo mutants on nextest; --in-diff <file> takes only the lines of a diff";
  tools =
    buildTools
    ++ testTools
    ++ [
      pkgs.bash
      pkgs.gawk
      pkgs.gnused
      pkgs.cargo-mutants
      pkgs.git
    ];
  # `dev` runs this from the root. Back to where it was called, so that a relative `--in-diff` path
  # means what it says, and `--dir` keeps the root as what gets mutated. `--workspace`, as from the
  # root cargo-mutants takes the root package alone, and a member would pass with no mutant tried.
  text = ''
    ${compileEnv}
    root="$PWD"
    cd "$OLDPWD"
    cargo mutants --dir "$root" --workspace "$@"
  '';
}
