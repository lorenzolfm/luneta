{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "run";
  usage = "[args...]";
  help = "the daemon, cargo run of luned";
  tools = buildTools ++ [ pkgs.git ];
  inheritPath = true;
  text = ''
    ${compileEnv}
    exec cargo run --locked -p luned --bin luned "$@"
  '';
}
