{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "run";
  usage = "[args...]";
  help = "the daemon, cargo run of lunetad, on your own zellij and claude";
  tools = buildTools ++ [ pkgs.git ];
  inheritPath = true;
  text = ''
    ${compileEnv}
    exec cargo run --locked -p lunetad --bin lunetad "$@"
  '';
}
