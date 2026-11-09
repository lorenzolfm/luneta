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
  # The daemon drives zellij and claude, which are the user's and not the
  # flake's: a zellij that is not the one whose sessions the daemon reads would
  # say nothing is there, and a claude that is not the one the user talks to
  # would answer for a person they are not. So the PATH it runs with is the
  # caller's, as `skills` runs the caller's claude, with the tools above first.
  inheritPath = true;
  # exec, so that ctrl-c and a SIGTERM reach the daemon, as they reach what `cargo run` runs.
  text = ''
    ${compileEnv}
    exec cargo run --locked -p lunetad --bin lunetad "$@"
  '';
}
