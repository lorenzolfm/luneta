{ pkgs }:
let
  inherit (pkgs) lib stdenv;
  version = "0.16.3";
  sources = {
    x86_64-linux = {
      target = "linux-x64";
      sha256 = "b66199f47ec18b00a47423cc8095dc06898eaed0c4f247a252cfce76388ea151";
    };
    aarch64-linux = {
      target = "linux-arm64";
      sha256 = "805201d715a6c684c8c1d5187d90639f077f8ffae68934e1197bcd0d13682037";
    };
    aarch64-darwin = {
      target = "darwin-arm64";
      sha256 = "42e894a411f54ca9a55237f6c654c9c7a90f45966ff74d4d9b36ae0d608dbe87";
    };
  };
  source =
    sources.${stdenv.hostPlatform.system}
      or (throw "terminal-browser has no release for ${stdenv.hostPlatform.system}");
  linuxLibs = [
    pkgs.alsa-lib
    pkgs.at-spi2-atk
    pkgs.at-spi2-core
    pkgs.cairo
    pkgs.cups
    pkgs.dbus
    pkgs.expat
    pkgs.glib
    pkgs.gtk3
    pkgs.libGL
    pkgs.libdrm
    pkgs.libgbm
    pkgs.libx11
    pkgs.libxcb
    pkgs.libxcomposite
    pkgs.libxdamage
    pkgs.libxext
    pkgs.libxfixes
    pkgs.libxkbcommon
    pkgs.libxrandr
    pkgs.libxshmfence
    pkgs.nspr
    pkgs.nss
    pkgs.pango
    pkgs.stdenv.cc.cc.lib
    pkgs.systemd
    pkgs.vulkan-loader
  ];
in
stdenv.mkDerivation {
  pname = "terminal-browser";
  inherit version;

  src = pkgs.fetchurl {
    url = "https://github.com/zenbu-labs/terminal-browser/releases/download/v${version}/terminal-browser-${source.target}.tar.gz";
    inherit (source) sha256;
  };

  sourceRoot = "terminal-browser";

  nativeBuildInputs = [
    pkgs.makeWrapper
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.autoPatchelfHook ];

  buildInputs = lib.optionals stdenv.hostPlatform.isLinux linuxLibs;

  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;

  preFixup = lib.optionalString stdenv.hostPlatform.isLinux ''
    addAutoPatchelfSearchPath "$out/libexec/terminal-browser/electron"
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p "$out/libexec/terminal-browser" "$out/bin"
    cp -a . "$out/libexec/terminal-browser/"
    makeWrapper "$out/libexec/terminal-browser/bin/terminal-browser" "$out/bin/terminal-browser" \
      --prefix PATH : "${lib.makeBinPath [ pkgs.openssh ]}" \
      ${lib.optionalString stdenv.hostPlatform.isLinux ''
        --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath linuxLibs}:$out/libexec/terminal-browser/electron"
      ''}
    runHook postInstall
  '';

  meta = {
    description = "Chromium inside Kitty-graphics terminals, packaged for lune";
    homepage = "https://github.com/zenbu-labs/terminal-browser";
    license = lib.licenses.mit;
    mainProgram = "terminal-browser";
    platforms = builtins.attrNames sources;
  };
}
