{ pkgs }:
let
  formatConfig = pkgs.writeText "swift-format.json" (
    builtins.toJSON {
      version = 1;
      lineLength = 100;
      indentation.spaces = 4;
      tabWidth = 4;
      maximumBlankLines = 1;
      respectsExistingLineBreaks = true;
    }
  );
in
{
  inherit formatConfig;

  format = pkgs.writeShellApplication {
    name = "swift-format-lune";
    runtimeInputs = [ pkgs.swift-format ];
    text = ''
      exec swift-format format --in-place --configuration ${formatConfig} -- "$@"
    '';
  };

  check = pkgs.writeShellApplication {
    name = "swift-check";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.diffutils
      pkgs.gnugrep
      pkgs.swift-format
      pkgs.swiftlint
    ];
    text = ''
      status=0
      for file in "$@"; do
        if ! swift-format format --configuration ${formatConfig} -- "$file" |
          diff --unified --label "$file" --label "$file, formatted" -- "$file" -; then
          echo "swift-check: swift-format would change $file: run dev ios fmt" >&2
          status=1
        fi
      done
      log="$(mktemp)"
      trap 'rm -f "$log"' EXIT
      { swiftlint lint --strict --quiet --no-cache --disable-sourcekit --config ios/.swiftlint.yml "$@" 2>&1 1>&3 |
        tee "$log" >&2; } 3>&1 || status=1
      if grep --quiet --fixed-strings 'because it requires SourceKit' "$log"; then
        echo "swift-check: ios/.swiftlint.yml turns on a rule above that needs SourceKit, which is off here: drop it" >&2
        status=1
      fi
      exit "$status"
    '';
  };
}
