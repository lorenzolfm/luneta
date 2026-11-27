{ root }:
let
  ciPath = ".github/workflows/ci.yml";
  ci = builtins.readFile (root + "/${ciPath}");

  entryKind =
    rel:
    let
      parent = dirOf rel;
      base = baseNameOf rel;
      dir = if parent == "." then root else root + "/${parent}";
    in
    if builtins.pathExists (root + "/${rel}") then (builtins.readDir dir).${base} or null else null;

  extractArray =
    name:
    let
      matched = builtins.match ".*\n[ ]*${name}=\\(\n([^)]*)\\).*" ci;
    in
    if matched == null then
      null
    else
      builtins.filter (item: item != "") (
        map (
          line:
          let
            trimmed = builtins.match "[ ]*([^ ]+)[ ]*" line;
          in
          if trimmed == null then "" else builtins.head trimmed
        ) (builtins.filter builtins.isString (builtins.split "\n" (builtins.head matched)))
      );

  iosDirs = extractArray "ios_dirs";
  iosFiles = extractArray "ios_files";

  cacheKeyMatch = builtins.match ".*key: cargo-[^\n]*hashFiles\\(([^)]*)\\).*" ci;
  cacheKeyFiles =
    if cacheKeyMatch == null then
      null
    else
      builtins.filter (item: item != "") (
        map (
          token:
          let
            unquoted = builtins.match "[ ]*'([^']+)'[ ]*" token;
          in
          if unquoted == null then "" else builtins.head unquoted
        ) (builtins.filter builtins.isString (builtins.split "," (builtins.head cacheKeyMatch)))
      );

  crateEntries = builtins.readDir (root + "/crates");
  allCrateDirs = builtins.filter (name: crateEntries.${name} == "directory") (
    builtins.attrNames crateEntries
  );

  depTablesOf =
    manifest:
    let
      targets = builtins.attrValues (manifest.target or { });
    in
    [
      (manifest.dependencies or { })
      (manifest.build-dependencies or { })
    ]
    ++ map (target: target.dependencies or { }) targets
    ++ map (target: target.build-dependencies or { }) targets;

  rawPathsOf =
    dir:
    let
      manifest = builtins.fromTOML (builtins.readFile (root + "/crates/${dir}/Cargo.toml"));
      deps = builtins.concatLists (map builtins.attrValues (depTablesOf manifest));
    in
    map (dep: dep.path) (builtins.filter (dep: builtins.isAttrs dep && dep ? path) deps);

  addUnique = list: item: if builtins.elem item list then list else list ++ [ item ];

  stepClosure =
    state: _:
    let
      paths = builtins.concatLists (map rawPathsOf state.dirs);
      parsed = map (
        path:
        let
          matched = builtins.match "\\.\\./([A-Za-z0-9_-]+)" path;
        in
        {
          inherit path;
          dir = if matched == null then null else builtins.head matched;
        }
      ) paths;
      bad = map (item: "crates Cargo.toml path dependency ${item.path} is not ../<crate>") (
        builtins.filter (item: item.dir == null || !(builtins.elem item.dir allCrateDirs)) parsed
      );
      found = map (item: item.dir) (
        builtins.filter (item: item.dir != null && builtins.elem item.dir allCrateDirs) parsed
      );
    in
    {
      dirs = builtins.foldl' addUnique state.dirs found;
      errors = builtins.foldl' addUnique state.errors bad;
    };

  closure = builtins.foldl' stepClosure {
    dirs = [
      "phone"
      "phone-bindgen"
    ];
    errors = [ ];
  } allCrateDirs;

  requiredDirs = [
    "ios"
    "proto"
  ]
  ++ map (dir: "crates/${dir}") closure.dirs;

  requiredFiles = [
    "Cargo.toml"
    "Cargo.lock"
    "rust-toolchain.toml"
    "flake.nix"
    "flake.lock"
    "nix/ios.nix"
    "nix/dev/default.nix"
    "nix/dev/commands/ios.nix"
    ciPath
  ];

  requiredCacheFiles = [
    "Cargo.toml"
    "Cargo.lock"
    "rust-toolchain.toml"
    "flake.lock"
  ]
  ++ map (dir: "crates/${dir}/Cargo.toml") closure.dirs;

  dirsList = if iosDirs == null then [ ] else iosDirs;
  filesList = if iosFiles == null then [ ] else iosFiles;
  cacheList = if cacheKeyFiles == null then [ ] else cacheKeyFiles;
in
closure.errors
++ (if dirsList == [ ] then [ "${ciPath}: missing or empty ios_dirs array" ] else [ ])
++ (if filesList == [ ] then [ "${ciPath}: missing or empty ios_files array" ] else [ ])
++ (
  if cacheList == [ ] then [ "${ciPath}: missing or empty ios cargo cache hashFiles(...)" ] else [ ]
)
++ map (dir: "${ciPath}: ios_dirs entry ${dir} is not a directory") (
  builtins.filter (dir: entryKind dir != "directory") dirsList
)
++ map (dir: "${ciPath}: ios_dirs entry ${dir} contains regex-unsafe characters") (
  builtins.filter (dir: builtins.match "[A-Za-z0-9_/-]+" dir == null) dirsList
)
++ map (file: "${ciPath}: ios_files entry ${file} is not a regular file") (
  builtins.filter (file: entryKind file != "regular") filesList
)
++ map (dir: "${ciPath}: ios_dirs is missing ${dir}") (
  builtins.filter (dir: !(builtins.elem dir dirsList)) requiredDirs
)
++ map (file: "${ciPath}: ios_files is missing ${file}") (
  builtins.filter (file: !(builtins.elem file filesList)) requiredFiles
)
++ map (file: "${ciPath}: ios cargo cache hashFiles(...) is missing ${file}") (
  builtins.filter (file: !(builtins.elem file cacheList)) requiredCacheFiles
)
