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
  help = "the tests on nextest, then the doctests; -P quick leaves out the one that waits 30s";
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
    case "''${TERM:-dumb}" in
      dumb) export NEXTEST_SHOW_PROGRESS="''${NEXTEST_SHOW_PROGRESS:-counter}" ;;
    esac
    cargo nextest run --locked ${testRun} "$@"
    cargo test --doc --locked ${testRun}
  '';
}
