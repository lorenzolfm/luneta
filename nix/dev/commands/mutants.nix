{
  pkgs,
  testKit,
  compileEnv,
  ...
}:
{
  name = "mutants";
  usage = "[args...]";
  help = "cargo mutants on nextest; --in-diff <file> takes only the lines of a diff";
  tools = testKit ++ [ pkgs.cargo-mutants ];
  text = ''
    ${compileEnv}
    root="$PWD"
    cd "$OLDPWD"
    cargo mutants --dir "$root" --workspace "$@"
  '';
}
