{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "record-status";
  usage = "<claude|pi|agy>";
  help = "drive the installed agent through each step of the status ladder in a terminal that answers OSC 7501, and write its rungs' recordings for luned's scenario suite; it spends model tokens, so CI never runs it";
  tools = buildTools ++ [ pkgs.coreutils ];
  inheritPath = true;
  text = ''
    ${compileEnv}
    if (( $# != 1 )); then
      echo "usage: dev record-status <claude|pi|agy>" >&2
      exit 2
    fi
    exec cargo run --locked --quiet -p luned --example record_status -- "$1"
  '';
}
