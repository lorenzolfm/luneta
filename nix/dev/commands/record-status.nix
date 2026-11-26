{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "record-status";
  usage = "<claude|pi|agy> [-- <launcher> [args...]]";
  help = "drive the installed agent, or the launcher that starts it for the turns that reach a model (it then picks the model), through each step of the status ladder in a terminal that answers OSC 7501, and write its rungs' recordings for luned's scenario suite; it spends model tokens, so CI never runs it";
  tools = buildTools ++ [ pkgs.coreutils ];
  inheritPath = true;
  text = ''
    ${compileEnv}
    if (( $# < 1 )); then
      echo "usage: dev record-status <claude|pi|agy> [-- <launcher> [args...]]" >&2
      exit 2
    fi
    exec cargo run --locked --quiet -p luned --example record_status -- "$@"
  '';
}
