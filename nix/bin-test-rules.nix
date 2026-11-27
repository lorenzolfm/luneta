{ root }:
let
  cargoToml = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
  members = cargoToml.workspace.members or [ ];
  exclude = cargoToml.workspace.exclude or [ ];
  cratesEntries = builtins.readDir (root + "/crates");
  crateDirs = builtins.filter (name: cratesEntries.${name} == "directory") (
    builtins.attrNames cratesEntries
  );
  stripDotSlash =
    path:
    if builtins.substring 0 2 path == "./" then
      stripDotSlash (builtins.substring 2 (builtins.stringLength path - 2) path)
    else
      path;
  firstExisting =
    dir: candidates:
    let
      found = builtins.filter (rel: builtins.pathExists (root + "/crates/${dir}/${rel}")) candidates;
    in
    if found == [ ] then null else builtins.head found;
  resolveBin =
    dir: packageName: bin:
    let
      explicit = if bin ? path then stripDotSlash bin.path else null;
      candidates =
        if bin.name == packageName then
          [
            "src/main.rs"
            "src/bin/${bin.name}.rs"
            "src/bin/${bin.name}/main.rs"
          ]
        else
          [
            "src/bin/${bin.name}.rs"
            "src/bin/${bin.name}/main.rs"
          ];
      rel = if explicit != null then explicit else firstExisting dir candidates;
    in
    if rel == null then
      throw "nix/bin-test-rules.nix: crates/${dir}/Cargo.toml [[bin]] ${bin.name} matches none of ${builtins.concatStringsSep ", " candidates}"
    else if !builtins.pathExists (root + "/crates/${dir}/${rel}") then
      throw "nix/bin-test-rules.nix: crates/${dir}/Cargo.toml [[bin]] ${bin.name} points to missing path crates/${dir}/${rel}"
    else
      "crates/${dir}/${rel}";
  untestedBins = builtins.concatLists (
    map (
      dir:
      let
        manifest = builtins.fromTOML (builtins.readFile (root + "/crates/${dir}/Cargo.toml"));
        packageName = manifest.package.name;
        bins = builtins.filter (bin: (bin.test or true) == false) (manifest.bin or [ ]);
      in
      map (resolveBin dir packageName) bins
    ) crateDirs
  );
  rule = {
    id = "untested-bins-hold-no-tests";
    language = "rust";
    message = "This [[bin]] sets test = false in Cargo.toml, so Cargo neither compiles nor runs tests in it. Put unit tests, submodules, and included files in the crate's library, or remove test = false.";
    rule.any = [
      {
        kind = "attribute";
        any = [
          { regex = ''^(::)?(\w+::)*test\b''; }
          { regex = ''^(cfg|cfg_attr)\((?s:.*\btest\b.*)\)$''; }
        ];
      }
      {
        kind = "mod_item";
        not.has = {
          field = "body";
          kind = "declaration_list";
        };
      }
      {
        kind = "macro_invocation";
        has = {
          field = "macro";
          regex = "^(::)?(core::|std::)?include$";
        };
      }
    ];
  }
  // (if untestedBins == [ ] then { ignores = [ "**" ]; } else { files = untestedBins; });
in
if members == [ "crates/*" ] && exclude == [ ] then
  builtins.toJSON rule
else
  throw "nix/bin-test-rules.nix: Cargo.toml workspace members changed; update nix/bin-test-rules.nix"
