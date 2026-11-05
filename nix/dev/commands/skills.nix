{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
{
  name = "skills";
  usage = "[dir]";
  help = "check the menu's skill finder against what claude runs in dir";
  tools = buildTools ++ [
    pkgs.bash
    pkgs.findutils
    pkgs.gawk
    pkgs.git
    pkgs.gnused
  ];
  inheritPath = true;
  text = ''
    ${compileEnv}
    dir="$(cd "''${1:-$DEV_HERE}" && pwd -P)"
    LUNETA_SKILLS_DIR="$dir" cargo test --locked -p lunetad --lib \
      skills::tests::the_finder_finds_what_claude_code_runs -- --ignored --exact
  '';
}
