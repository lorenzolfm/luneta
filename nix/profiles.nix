{ root }:
let
  inherit (builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"))) profile;
  locked =
    map (package: package.name)
      (builtins.fromTOML (builtins.readFile (root + "/Cargo.lock"))).package;
  levels =
    name: builtins.mapAttrs (_: package: package.opt-level or null) (profile.${name}.package or { });
  dev = levels "dev";
  release = levels "release";
  checks = profile.checks or null;
  checksPackage = if checks == null then { } else checks.package or { };
  overridden = builtins.attrNames (dev // release);
  native = builtins.filter (
    name:
    builtins.elem name [
      "transcribe-cpp"
      "transcribe-cpp-sys"
    ]
  ) overridden;
  missing = builtins.filter (name: !builtins.elem name locked) overridden;
  checksFailures =
    if checks == null then
      [
        "Cargo.toml: [profile.checks] is missing; flake.nix builds checks.<system>.luned and clippy in it"
      ]
    else
      (
        if (checks.inherits or null) == "dev" then
          [ ]
        else
          [
            "Cargo.toml: [profile.checks] must set inherits = \"dev\" so CI runs with debug_assert!, overflow-checks and dev's opt-level 3 crate overrides"
          ]
      )
      ++ (
        if checksPackage == { } then
          [ ]
        else
          [
            "Cargo.toml: [profile.checks.package.*] overrides packages directly; keep per-crate opt-levels in [profile.dev.package.*] and [profile.release.package.*] so checks inherits them"
          ]
      )
      ++ (
        if (checks.debug-assertions or true) == true then
          [ ]
        else
          [
            "Cargo.toml: [profile.checks] turns debug-assertions off; CI tests in checks so debug_assert! stays active"
          ]
      )
      ++ (
        if (checks.overflow-checks or true) == true then
          [ ]
        else
          [
            "Cargo.toml: [profile.checks] turns overflow-checks off; CI tests in checks so arithmetic overflow stays checked"
          ]
      )
      ++ (
        if
          builtins.elem (checks.lto or false) [
            false
            "off"
          ]
        then
          [ ]
        else
          [
            "Cargo.toml: [profile.checks] enables lto; CI tests in checks to avoid LTO link times"
          ]
      );
in
(
  if dev == release then
    [ ]
  else
    [
      "Cargo.toml: [profile.dev.package.*] and [profile.release.package.*] set different opt-levels; the speech crates run at opt-level 3 in both profiles, so change them together"
    ]
)
++ checksFailures
++ map (
  name:
  "Cargo.toml: ${name} gets a profile override, but transcribe-cpp-sys builds its native core with CMake Release whatever the Rust profile, so the override changes nothing"
) native
++ map (
  name:
  "Cargo.toml: ${name} gets a profile override, but no package in Cargo.lock has that name, so the override changes nothing"
) missing
