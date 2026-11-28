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
  nextest = builtins.fromTOML (builtins.readFile (root + "/.config/nextest.toml"));
  nextestTopLevelFailures = map (
    key: ".config/nextest.toml: unknown top-level key ${builtins.toJSON key}"
  ) (builtins.filter (key: key != "profile") (builtins.attrNames nextest));
  nextestProfilesRaw = nextest.profile or null;
  nextestProfiles = if builtins.isAttrs nextestProfilesRaw then nextestProfilesRaw else { };
  allowedNextestProfiles = {
    default = [
      "fail-fast"
      "overrides"
    ];
    quick = [
      "default-filter"
      "overrides"
    ];
    ci = [
      "retries"
      "slow-timeout"
      "failure-output"
      "overrides"
    ];
  };
  allowedOverrideKeys = [
    "filter"
    "priority"
    "slow-timeout"
  ];
  missingNextestProfiles = builtins.concatLists (
    map (
      profileName:
      if nextestProfiles ? ${profileName} then
        [ ]
      else
        [ ".config/nextest.toml: missing [profile.${profileName}] table required by flake.nix and CI" ]
    ) (builtins.attrNames allowedNextestProfiles)
  );
  nextestFailures =
    nextestTopLevelFailures
    ++ (
      if builtins.isAttrs nextestProfilesRaw then
        [ ]
      else
        [ ".config/nextest.toml: [profile] must be a table" ]
    )
    ++ missingNextestProfiles
    ++ builtins.concatLists (
      map (
        profileName:
        let
          profileAttrs = nextestProfiles.${profileName};
          allowedKeys = allowedNextestProfiles.${profileName} or null;
          unknownProfile =
            if allowedKeys != null then
              [ ]
            else
              [
                ".config/nextest.toml: unknown profile [profile.${profileName}]; keep nix/profiles.nix and daemon host.rs schema aligned"
              ];
          nonTable =
            if builtins.isAttrs profileAttrs then
              [ ]
            else
              [ ".config/nextest.toml: [profile.${profileName}] must be a table" ];
          profileRetries =
            if !builtins.isAttrs profileAttrs then
              [ ]
            else if profileName == "ci" then
              if (profileAttrs.retries or null) == 0 then
                [ ]
              else
                [
                  ".config/nextest.toml: [profile.ci].retries must be 0 so CI does not hide flaky tests (got ${
                    builtins.toJSON (profileAttrs.retries or null)
                  })"
                ]
            else if profileAttrs ? retries then
              [
                ".config/nextest.toml: [profile.${profileName}] sets retries = ${builtins.toJSON profileAttrs.retries}; keep retries disabled so CI does not hide flaky tests"
              ]
            else
              [ ];
          unknownKeys =
            if allowedKeys == null || !builtins.isAttrs profileAttrs then
              [ ]
            else
              map (key: ".config/nextest.toml: [profile.${profileName}] must not set ${key}") (
                builtins.filter (
                  key: !(builtins.elem key allowedKeys) && !(profileName != "ci" && key == "retries")
                ) (builtins.attrNames profileAttrs)
              );
          overridesRaw = if builtins.isAttrs profileAttrs then profileAttrs.overrides or [ ] else [ ];
          overrideFailures =
            if !builtins.isList overridesRaw then
              [ ".config/nextest.toml: [profile.${profileName}].overrides must be an array of tables" ]
            else
              builtins.concatLists (
                map (
                  entry:
                  if !builtins.isAttrs entry then
                    [ ".config/nextest.toml: [profile.${profileName}.overrides] contains a non-table entry" ]
                  else
                    (
                      if entry ? retries then
                        [
                          ".config/nextest.toml: [profile.${profileName}.overrides] sets retries = ${builtins.toJSON entry.retries}; keep retries disabled so CI does not hide flaky tests"
                        ]
                      else
                        [ ]
                    )
                    ++ map (key: ".config/nextest.toml: [profile.${profileName}.overrides] must not set ${key}") (
                      builtins.filter (key: !(builtins.elem key allowedOverrideKeys) && key != "retries") (
                        builtins.attrNames entry
                      )
                    )
                ) overridesRaw
              );
        in
        unknownProfile ++ nonTable ++ profileRetries ++ unknownKeys ++ overrideFailures
      ) (builtins.attrNames nextestProfiles)
    );
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
    ++ checkDrv "clippyRelease" "release"
    ++ checkDrv "clippyCrossRelease" "release"
    ++ checkDrv "releaseDeps" "release"
    ++ checkDrv "luned" "release";
  devFailures =
    if devProfile ? opt-level then
      [
        "Cargo.toml: [profile.dev] must not override opt-level so checks inherit unoptimized dev builds"
      ]
    else
      [ ];
  releaseFailures =
    if !(profile ? release) then
      [
        "Cargo.toml: [profile.release] is missing; flake.nix builds packages.<system>.luned with CARGO_PROFILE = \"release\""
      ]
    else
      (
        if (releaseProfile.lto or null) == true then
          [ ]
        else
          [
            "Cargo.toml: [profile.release] must explicitly set lto = true for shipped release binaries"
          ]
      )
      ++ (
        if (releaseProfile.strip or null) == true then
          [ ]
        else
          [
            "Cargo.toml: [profile.release] must explicitly set strip = true for shipped release binaries"
          ]
      )
      ++ (
        if (releaseProfile.debug-assertions or false) == false then
          [ ]
        else
          [
            "Cargo.toml: [profile.release] must not enable debug-assertions"
          ]
      );
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
        if checks ? opt-level then
          [
            "Cargo.toml: [profile.${checksProfile}] must not override opt-level so CI tests unoptimized dev builds"
          ]
        else
          [ ]
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
++ devFailures
++ checksFailures
++ releaseFailures
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
++ nextestFailures
