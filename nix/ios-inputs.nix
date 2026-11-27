{ root }:
let
  ciPath = ".github/workflows/ci.yml";
  ci = builtins.readFile (root + "/${ciPath}");

  isCanonicalDir = rel: builtins.match "[A-Za-z0-9_-]+(/[A-Za-z0-9_-]+)*" rel != null;

  isCanonicalFile =
    rel:
    builtins.match "\\.?[A-Za-z0-9_-][A-Za-z0-9_.-]*(/\\.?[A-Za-z0-9_-][A-Za-z0-9_.-]*)*" rel != null;

  entryKind =
    rel:
    let
      parent = dirOf rel;
      base = baseNameOf rel;
      dir = if parent == "." then root else root + "/${parent}";
    in
    if builtins.pathExists (root + "/${rel}") then (builtins.readDir dir).${base} or null else null;

  extractUnique =
    pattern: label:
    let
      matches = builtins.filter builtins.isList (builtins.split pattern ci);
    in
    if matches == [ ] then
      {
        raw = null;
        errors = [ "${ciPath}: missing ${label}" ];
      }
    else if builtins.length matches > 1 then
      {
        raw = null;
        errors = [ "${ciPath}: multiple ${label} blocks found" ];
      }
    else
      {
        raw = builtins.head (builtins.head matches);
        errors = [ ];
      };

  extractArray =
    name:
    let
      found = extractUnique "\n[ ]*${name}=\\(\n([^)]*)\\)" "${name} array";
      nonBlank =
        if found.raw == null then
          [ ]
        else
          builtins.filter (line: builtins.match "[ \t\r]*" line == null) (
            builtins.filter builtins.isString (builtins.split "\n" found.raw)
          );
      parsed = map (
        line:
        let
          m = builtins.match "[ \t]*([^ \t\r]+)[ \t\r]*" line;
        in
        if m == null then
          {
            item = null;
            error = "${ciPath}: ${name} line must hold one path: ${line}";
          }
        else
          {
            item = builtins.head m;
            error = null;
          }
      ) nonBlank;
      items = map (p: p.item) (builtins.filter (p: p.item != null) parsed);
    in
    {
      inherit items;
      errors =
        found.errors
        ++ map (p: p.error) (builtins.filter (p: p.error != null) parsed)
        ++ (if found.raw != null && items == [ ] then [ "${ciPath}: empty ${name} array" ] else [ ]);
    };

  extractCacheKey =
    prefix:
    let
      lineFound = extractUnique "\n[ ]*key: (${prefix}[^\n]*)" "${prefix} cargo cache key";
      hashCalls =
        if lineFound.raw == null then
          [ ]
        else
          builtins.filter builtins.isList (builtins.split "hashFiles\\(" lineFound.raw);
      hashMatches =
        if lineFound.raw == null then
          [ ]
        else
          builtins.filter builtins.isList (builtins.split "hashFiles\\(([^)]*)\\)" lineFound.raw);
      hashErrors =
        if lineFound.raw == null then
          [ ]
        else if hashMatches == [ ] then
          [ "${ciPath}: missing ${prefix} cargo cache hashFiles(...)" ]
        else if builtins.length hashCalls > 1 || builtins.length hashMatches > 1 then
          [ "${ciPath}: multiple ${prefix} cargo cache hashFiles(...) calls found" ]
        else
          [ ];
      raw =
        if hashErrors == [ ] && hashMatches != [ ] then
          builtins.head (builtins.head hashMatches)
        else
          null;
      isEmpty = raw != null && builtins.match "[ \t]*" raw != null;
      tokens =
        if raw == null || isEmpty then
          [ ]
        else
          builtins.filter builtins.isString (builtins.split "," raw);
      parsed = map (
        token:
        let
          m = builtins.match "[ ]*'([^']+)'[ ]*" token;
        in
        if m == null then
          {
            item = null;
            error = "${ciPath}: unparseable ${prefix} hashFiles(...) entry: ${token}";
          }
        else
          {
            item = builtins.head m;
            error = null;
          }
      ) tokens;
      items = map (p: p.item) (builtins.filter (p: p.item != null) parsed);
    in
    {
      inherit items;
      errors =
        lineFound.errors
        ++ hashErrors
        ++ (if isEmpty then [ "${ciPath}: empty ${prefix} cargo cache hashFiles(...)" ] else [ ])
        ++ map (p: p.error) (builtins.filter (p: p.error != null) parsed);
    };

  iosDirs = extractArray "ios_dirs";
  iosFiles = extractArray "ios_files";
  registryCache = extractCacheKey "cargo-registry-";
  targetCache = extractCacheKey "cargo-target-";

  crateEntries = builtins.readDir (root + "/crates");
  allCrateDirs = builtins.filter (name: crateEntries.${name} == "directory") (
    builtins.attrNames crateEntries
  );

  rootManifest = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
  workspaceDeps = rootManifest.workspace.dependencies or { };

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

  depEntriesOf =
    manifest:
    builtins.concatLists (
      map (
        table:
        map (name: {
          inherit name;
          spec = table.${name};
        }) (builtins.attrNames table)
      ) (depTablesOf manifest)
    );

  pathRefsOf =
    dir:
    let
      manifest = builtins.fromTOML (builtins.readFile (root + "/crates/${dir}/Cargo.toml"));
      classify =
        entry:
        let
          inherit (entry) name spec;
        in
        if builtins.isAttrs spec && spec ? path then
          {
            kind = "crate";
            inherit (spec) path;
            error = null;
          }
        else if builtins.isAttrs spec && (spec.workspace or false) then
          let
            ws = workspaceDeps.${name} or null;
          in
          if ws == null then
            {
              kind = "error";
              path = null;
              error = "crates/${dir}/Cargo.toml: workspace dependency ${name} missing from Cargo.toml [workspace.dependencies]";
            }
          else if builtins.isAttrs ws && ws ? path then
            {
              kind = "workspace";
              inherit (ws) path;
              error = null;
            }
          else
            null
        else
          null;
    in
    builtins.filter (item: item != null) (map classify (depEntriesOf manifest));

  addUnique = list: item: if builtins.elem item list then list else list ++ [ item ];

  stepClosure =
    state: _:
    let
      refs = builtins.concatLists (map pathRefsOf state.dirs);
      parsed = map (
        ref:
        if ref.kind == "error" then
          {
            dir = null;
            inherit (ref) error;
          }
        else
          let
            pattern =
              if ref.kind == "workspace" then "crates/([A-Za-z0-9_-]+)" else "\\.\\./([A-Za-z0-9_-]+)";
            expected = if ref.kind == "workspace" then "crates/<crate>" else "../<crate>";
            matched = builtins.match pattern ref.path;
            dir = if matched == null then null else builtins.head matched;
          in
          if dir == null || !(builtins.elem dir allCrateDirs) then
            {
              dir = null;
              error = "Cargo.toml path dependency ${ref.path} is not ${expected}";
            }
          else
            {
              inherit dir;
              error = null;
            }
      ) refs;
      bad = map (item: item.error) (builtins.filter (item: item.error != null) parsed);
      found = map (item: item.dir) (builtins.filter (item: item.dir != null) parsed);
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

  cargoBuildFiles = [
    "Cargo.toml"
    "Cargo.lock"
    "rust-toolchain.toml"
    "flake.nix"
    "flake.lock"
    "nix/dev/default.nix"
    "nix/dev/commands/ios.nix"
    ciPath
  ];

  requiredDirs = [
    "ios"
    "proto"
  ]
  ++ map (dir: "crates/${dir}") closure.dirs;

  requiredFiles = cargoBuildFiles ++ [ "nix/ios.nix" ];

  requiredRegistryCacheFiles = [ "Cargo.lock" ];

  requiredTargetCacheFiles = cargoBuildFiles ++ map (dir: "crates/${dir}/Cargo.toml") closure.dirs;

  validateFileEntries =
    label: items:
    map (file: "${ciPath}: ${label} entry ${file} is not a canonical relative path") (
      builtins.filter (file: !(isCanonicalFile file)) items
    )
    ++ map (file: "${ciPath}: ${label} entry ${file} is not a regular file") (
      builtins.filter (file: isCanonicalFile file && entryKind file != "regular") items
    );
in
closure.errors
++ iosDirs.errors
++ iosFiles.errors
++ registryCache.errors
++ targetCache.errors
++ map (dir: "${ciPath}: ios_dirs entry ${dir} is not a canonical relative path") (
  builtins.filter (dir: !(isCanonicalDir dir)) iosDirs.items
)
++ map (dir: "${ciPath}: ios_dirs entry ${dir} is not a directory") (
  builtins.filter (dir: isCanonicalDir dir && entryKind dir != "directory") iosDirs.items
)
++ validateFileEntries "ios_files" iosFiles.items
++ validateFileEntries "cargo-registry- hashFiles(...)" registryCache.items
++ validateFileEntries "cargo-target- hashFiles(...)" targetCache.items
++ map (dir: "${ciPath}: ios_dirs is missing ${dir}") (
  builtins.filter (dir: !(builtins.elem dir iosDirs.items)) requiredDirs
)
++ map (file: "${ciPath}: ios_files is missing ${file}") (
  builtins.filter (file: !(builtins.elem file iosFiles.items)) requiredFiles
)
++ map (file: "${ciPath}: cargo-registry- hashFiles(...) is missing ${file}") (
  builtins.filter (file: !(builtins.elem file registryCache.items)) requiredRegistryCacheFiles
)
++ map (file: "${ciPath}: cargo-target- hashFiles(...) is missing ${file}") (
  builtins.filter (file: !(builtins.elem file targetCache.items)) requiredTargetCacheFiles
)
