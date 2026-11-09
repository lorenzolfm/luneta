{
  compileEnv,
  testRun,
  testKit,
  ...
}:
{
  name = "test";
  usage = "[args...]";
  help = "the tests on nextest, then the doctests; -P quick leaves out the two that wait 30s";
  tools = testKit;
  text = ''
    ${compileEnv}
    case "''${TERM:-dumb}" in
      dumb) export NEXTEST_SHOW_PROGRESS="''${NEXTEST_SHOW_PROGRESS:-counter}" ;;
    esac
    cargo nextest run --locked ${testRun} "$@"
    cargo test --doc --locked ${testRun}
  '';
}
