{
  compileEnv,
  testRun,
  nextest,
  testKit,
  ...
}:
let
  nextestConfig = builtins.fromTOML (builtins.readFile ../../../.config/nextest.toml);
  quickFilter =
    nextestConfig.profile.quick.default-filter
      or (throw ".config/nextest.toml has no [profile.quick].default-filter");
  quickParts = builtins.filter builtins.isString (
    builtins.split "[[:space:]]+and[[:space:]]+" quickFilter
  );
  quickExcluded = builtins.filter (
    part: builtins.match "[[:space:]]*not[[:space:]]+test\\(=[a-zA-Z0-9_:]+\\)[[:space:]]*" part != null
  ) quickParts;
  quickCount =
    if quickParts == [ ] || builtins.length quickExcluded != builtins.length quickParts then
      throw ".config/nextest.toml [profile.quick].default-filter must be `not test(=...)` clauses joined by `and`"
    else
      builtins.length quickExcluded;
  quickPhrase =
    if quickCount == 1 then "1 test that waits" else "${toString quickCount} tests that wait";
in
{
  name = "test";
  usage = "[args...]";
  help = "the tests on nextest, then the doctests; -P quick leaves out the ${quickPhrase} 30s and more; fails if the tests leave a lune-mux running";
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
