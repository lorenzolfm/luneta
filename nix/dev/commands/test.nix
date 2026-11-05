{
  pkgs,
  buildTools,
  compileEnv,
  testRun,
  testTools,
  ...
}:
{
  name = "test";
  usage = "[args...]";
  help = "the tests, on nextest; -P quick leaves out those that wait 30s";
  tools =
    buildTools
    ++ testTools
    ++ [
      pkgs.bash
      pkgs.gawk
      pkgs.gnused
    ];
  text = ''
    ${compileEnv}
    cargo nextest run --locked ${testRun} "$@"
  '';
}
