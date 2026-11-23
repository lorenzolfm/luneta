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
  help = "the tests on nextest, then the doctests; -P quick leaves out the two that wait 30s and more; fails if the tests leave a lune-mux running";
  tools = testKit ++ [ nextest ];
  text = ''
    ${compileEnv}
    case "''${TERM:-dumb}" in
      dumb) export NEXTEST_SHOW_PROGRESS="''${NEXTEST_SHOW_PROGRESS:-counter}" ;;
    esac
    lune-nextest "$@"
    cargo test --doc --locked ${testRun}
  '';
}
