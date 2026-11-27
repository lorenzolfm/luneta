{ pkgs, root }:
let
  inherit (pkgs) lib;
  crateDirs = builtins.attrNames (
    lib.filterAttrs (_: kind: kind == "directory") (builtins.readDir (root + "/crates"))
  );
  untestedBins = lib.concatMap (
    dir:
    let
      manifest = builtins.fromTOML (builtins.readFile (root + "/crates/${dir}/Cargo.toml"));
      packageName = manifest.package.name;
      bins = builtins.filter (bin: (bin.test or true) == false) (manifest.bin or [ ]);
      binPath =
        bin:
        lib.removePrefix "./" (
          bin.path or (if bin.name == packageName then "src/main.rs" else "src/bin/${bin.name}.rs")
        );
    in
    map (bin: "crates/${dir}/${binPath bin}") bins
  ) crateDirs;
  rules = lib.optionals (untestedBins != [ ]) [
    {
      id = "untested-bins-hold-no-tests";
      language = "rust";
      files = untestedBins;
      message = "This [[bin]] sets test = false in Cargo.toml, so Cargo neither compiles nor runs tests in it. Put unit tests and submodules in the crate's library, or remove test = false.";
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
      ];
    }
  ];
in
pkgs.writeTextDir "bin-tests.yml" (lib.concatMapStringsSep "\n---\n" builtins.toJSON rules)
