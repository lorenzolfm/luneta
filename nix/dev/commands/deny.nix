{ pkgs, toolchain, ... }:
{
  name = "deny";
  usage = "";
  help = "check Cargo.lock against RustSec advisories";
  tools = [
    toolchain
    pkgs.cargo-deny
    pkgs.git
  ];
  text = ''
    cargo deny check advisories
  '';
}
