{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "run";
  usage = "[args...]";
  help = "the daemon, cargo run of lunetad, with a debug build of what it builds";
  tools = buildTools ++ [ pkgs.git ];
  # exec, so that ctrl-c and a SIGTERM reach the daemon, as they reach what `cargo run` runs.
  text = ''
    ${compileEnv}
    exec cargo run --locked -p lunetad --bin lunetad "$@"
  '';
}
