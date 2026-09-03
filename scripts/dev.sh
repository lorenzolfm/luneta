# luneta build recipes, as one command.
#
# Lives in the devshell and behind `nix run .#dev`. The rust toolchain comes from
# runtimeInputs, so every subcommand builds with the version flake.nix pins whether
# or not the caller entered the shell first. The system rustc cannot build this
# crate; see flake.nix.
#
# writeShellApplication supplies the shebang and `set -euo pipefail`, and lints this
# file with shellcheck at build time.

usage() {
  cat <<'HELP'
dev — luneta build recipes (pinned nix tooling)

  dev build                  cargo build --release --target wasm32-wasip1
  dev install                build, then install to ~/.local/share/zellij/plugins
  dev reload [session]       install, then put the new bytes in the running picker
  dev clean                  cargo clean
HELP
}

cmd="${1:-help}"
shift || true

case "$cmd" in
  help | -h | --help)
    usage
    exit 0
    ;;
  build | install | reload | clean) ;;
  *)
    # Explicit failure, not a silent fall-through to help: `dev buidl` in a script
    # or a CI step must not exit 0.
    printf 'dev: unknown command %s\n\n' "$cmd" >&2
    usage >&2
    exit 2
    ;;
esac

if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  echo "dev: run this inside the luneta checkout (no git worktree here)" >&2
  exit 2
fi
cd "$root"

wasm="target/wasm32-wasip1/release/luneta.wasm"

# Zellij caches permissions against the absolute path of the .wasm. Keep this path
# stable — including the literal ~/.local/share, rather than XDG_DATA_HOME, which
# would move the file for anyone who sets it — or zellij asks for the permissions
# again after each install.
install_dir="$HOME/.local/share/zellij/plugins"
installed="$install_dir/luneta.wasm"

build() {
  cargo build --release --target wasm32-wasip1
}

do_install() {
  build
  mkdir -p "$install_dir"
  install -m 0644 "$wasm" "$installed"
  echo "installed -> $installed"
}

case "$cmd" in
  build)
    build
    ;;

  install)
    do_install
    ;;

  # Installing first is not optional here: a reload that reads the old bytes off
  # disk is the failure this command exists to avoid, and the copy is free.
  #
  # zellij is deliberately not a runtimeInput. These calls drive the server that is
  # already running, and a pinned client that drifts from it fails on a version
  # mismatch instead of reloading. It comes from the ambient PATH.
  #
  # Two calls are necessary, because neither one covers both states:
  #
  #   launch-or-focus  Creates the picker if it does not run, and is the only call
  #                    that can pass --floating, which is the geometry the key
  #                    binding gives it. On a picker that runs, it takes focus.
  #   start-or-reload  Reads the .wasm from disk again and puts it into the pane
  #                    that is already there. This is what loads the new bytes.
  #
  # The order matters. Focus or create first, so that there is something to reload.
  #
  # Do not return to a close-and-relaunch pair. That version had to name the pane it
  # closed, and it could not: zellij documents a `plugin_<id>` on the stdout of
  # launch-or-focus-plugin ("Returns: Plugin pane ID"), but 0.45.1 prints nothing.
  # The guard thus saw an empty id and refused to run, which was correct:
  # `close-pane` without an id closes the focused pane, which during development is
  # usually your editor. A reload needs no pane id.
  #
  # Pass a session name to drive another session. Omit it to use the current one.
  reload)
    do_install

    zellij=(zellij)
    if [ "$#" -gt 0 ] && [ -n "$1" ]; then
      zellij=(zellij -s "$1")
    fi

    "${zellij[@]}" action launch-or-focus-plugin \
      --skip-plugin-cache --floating "file:$installed"
    "${zellij[@]}" action start-or-reload-plugin "file:$installed"
    ;;

  clean)
    cargo clean
    ;;
esac
