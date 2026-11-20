{
  compileEnv,
  testRun,
  nextest,
  testKit,
  ...
}:
{
  name = "test";
  usage = "[args...]";
  help = "the tests on nextest, then the doctests; -P quick leaves out the two that wait 30s and more; fails if the tests leave a luneta-mux running";
  tools = testKit ++ [ nextest ];
  text = ''
    ${compileEnv}
    case "''${TERM:-dumb}" in
      dumb) export NEXTEST_SHOW_PROGRESS="''${NEXTEST_SHOW_PROGRESS:-counter}" ;;
    esac
    luneta-nextest "$@"
    cargo test --doc --locked ${testRun}
  '';
}
