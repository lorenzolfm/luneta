{ root }:
let
  inherit (builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"))) profile;
  locked = map (package: package.name) (builtins.fromTOML (builtins.readFile (root + "/Cargo.lock")))
  .package;
  levels =
    name: builtins.mapAttrs (_: package: package.opt-level or null) (profile.${name}.package or { });
  dev = levels "dev";
  release = levels "release";
  overridden = builtins.attrNames (dev // release);
  native = builtins.filter (
    name:
    builtins.elem name [
      "transcribe-cpp"
      "transcribe-cpp-sys"
    ]
  ) overridden;
  missing = builtins.filter (name: !builtins.elem name locked) overridden;
in
(
  if dev == release then
    [ ]
  else
    [
      "Cargo.toml: [profile.dev.package.*] and [profile.release.package.*] set different opt-levels; the speech crates run at opt-level 3 in both profiles, so change them together"
    ]
)
++ map (
  name:
  "Cargo.toml: ${name} gets a profile override, but transcribe-cpp-sys builds its native core with CMake Release whatever the Rust profile, so the override changes nothing"
) native
++ map (
  name:
  "Cargo.toml: ${name} gets a profile override, but no package in Cargo.lock has that name, so the override changes nothing"
) missing
