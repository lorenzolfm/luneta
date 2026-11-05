{
  pkgs,
  buildTools,
  compileEnv,
  clippyRun,
  ...
}:
{
  name = "clippy";
  usage = "[args...]";
  help = "cargo clippy over every target, warnings denied";
  tools = buildTools ++ [ pkgs.git ];
  text = ''
    ${compileEnv}
    cargo clippy --locked ${clippyRun} "$@" -- -D warnings
  '';
}
