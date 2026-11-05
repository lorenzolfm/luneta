{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "check";
  usage = "[args...]";
  help = "cargo check over every target, without clippy's lints";
  tools = buildTools ++ [ pkgs.git ];
  text = ''
    ${compileEnv}
    cargo check --locked --all-targets --workspace "$@"
  '';
}
