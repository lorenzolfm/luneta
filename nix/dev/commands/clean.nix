{ toolchain, ... }:
{
  name = "clean";
  usage = "";
  help = "cargo clean";
  tools = [ toolchain ];
  text = ''
    cargo clean
  '';
}
