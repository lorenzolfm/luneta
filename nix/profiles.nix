{
  root,
  checksProfile ? "checks",
  derivationProfiles ? { },
}:
let
  inherit (builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"))) profile;
  locked =
    map (package: package.name)
      (builtins.fromTOML (builtins.readFile (root + "/Cargo.lock"))).package;
  devProfile = profile.dev or { };
  releaseProfile = profile.release or { };
  devPackages = devProfile.package or { };
  releasePackages = releaseProfile.package or { };
  levels = builtins.mapAttrs (_: package: package.opt-level or null);
  dev = levels devPackages;
  release = levels releasePackages;
  checks = if builtins.isString checksProfile then profile.${checksProfile} or null else null;
  effective = key: default: checks.${key} or (devProfile.${key} or default);
  overridden = builtins.attrNames (dev // release);
  extraPackageKeys = builtins.filter (
    name:
    let
      devKeys = builtins.attrNames (devPackages.${name} or { });
      releaseKeys = builtins.attrNames (releasePackages.${name} or { });
    in
    (devKeys != [ ] && devKeys != [ "opt-level" ])
    || (releaseKeys != [ ] && releaseKeys != [ "opt-level" ])
  ) overridden;
  native = builtins.filter (
    name:
    builtins.elem name [
      "transcribe-cpp"
      "transcribe-cpp-sys"
    ]
  ) overridden;
  missing = builtins.filter (name: !builtins.elem name locked) overridden;
  cratesEntries = builtins.readDir (root + "/crates");
  crateDirs = builtins.filter (name: cratesEntries.${name} == "directory") (
    builtins.attrNames cratesEntries
  );
  untestedLibs = builtins.filter (
    dir:
    let
      manifest = builtins.fromTOML (builtins.readFile (root + "/crates/${dir}/Cargo.toml"));
      lib = manifest.lib or { };
    in
    (lib.test or true) == false || (lib.doctest or true) == false
  ) crateDirs;
  derivationFailures =
    let
      checkDrv =
        name: expected:
        if !(derivationProfiles ? ${name}) then
          [ ]
        else if derivationProfiles.${name} == expected then
          [ ]
        else
          [
            "flake.nix: ${name} sets CARGO_PROFILE = ${
              builtins.toJSON derivationProfiles.${name}
            }, expected ${builtins.toJSON expected}"
          ];
    in
    checkDrv "checksDeps" checksProfile
    ++ checkDrv "tested" checksProfile
    ++ checkDrv "clippy" checksProfile
    ++ checkDrv "releaseDeps" "release"
    ++ checkDrv "luned" "release";
  checksFailures =
    if
      !(builtins.isString checksProfile)
      || builtins.elem checksProfile [
        "dev"
        "release"
      ]
    then
      [
        "flake.nix: checksProfile must name a dedicated checks profile, got ${builtins.toJSON checksProfile}"
      ]
    else if checks == null then
      [
        "Cargo.toml: [profile.${checksProfile}] is missing; flake.nix builds checks.<system>.luned and clippy in it"
      ]
    else
      (
        if (checks.inherits or null) == "dev" then
          [ ]
        else
          [
            "Cargo.toml: [profile.${checksProfile}] must set inherits = \"dev\" so CI runs with debug_assert!, overflow-checks and dev's opt-level 3 crate overrides"
          ]
      )
      ++ (
        if (checks.package or { }) == { } && (checks.build-override or { }) == { } then
          [ ]
        else
          [
            "Cargo.toml: [profile.${checksProfile}] overrides packages or build-override directly; keep per-crate opt-levels in [profile.dev.package.*] and [profile.release.package.*] so ${checksProfile} inherits them"
          ]
      )
      ++ (
        if effective "debug-assertions" true == true then
          [ ]
        else
          [
            "Cargo.toml: effective debug-assertions for [profile.${checksProfile}] is off; keep debug-assertions enabled across [profile.dev] and [profile.${checksProfile}] so debug_assert! stays active"
          ]
      )
      ++ (
        if effective "overflow-checks" true == true then
          [ ]
        else
          [
            "Cargo.toml: effective overflow-checks for [profile.${checksProfile}] is off; keep overflow-checks enabled across [profile.dev] and [profile.${checksProfile}] so arithmetic overflow stays checked"
          ]
      )
      ++ (
        if
          builtins.elem (effective "lto" false) [
            false
            "off"
          ]
        then
          [ ]
        else
          [
            "Cargo.toml: effective lto for [profile.${checksProfile}] is enabled; keep lto off across [profile.dev] and [profile.${checksProfile}] to avoid LTO link times"
          ]
      )
      ++ (
        if !(profile ? test) then
          [ ]
        else
          [
            "Cargo.toml: [profile.test] overrides the default test profile; keep local dev test aligned with [profile.dev] and [profile.${checksProfile}]"
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
++ derivationFailures
++ map (
  name:
  "Cargo.toml: [profile.*.package.${name}] sets keys besides opt-level; keep package overrides to opt-level only so debug-assertions and overflow-checks stay active"
) extraPackageKeys
++ map (
  dir:
  "crates/${dir}/Cargo.toml: [lib] disables test or doctest; library crates must keep unit tests and doctests enabled"
) untestedLibs
++ map (
  name:
  "Cargo.toml: ${name} gets a profile override, but transcribe-cpp-sys builds its native core with CMake Release whatever the Rust profile, so the override changes nothing"
) native
++ map (
  name:
  "Cargo.toml: ${name} gets a profile override, but no package in Cargo.lock has that name, so the override changes nothing"
) missing
