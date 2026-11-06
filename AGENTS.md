# luneta

Run cargo inside `nix develop`: the host's cargo can't build `alsa-sys`.

`dev` comes with that shell. Outside it, `nix run .#dev -- <command>`. `dev help` lists every command.

Before a commit, run `dev fmt`, `dev check`, `dev clippy`, `dev lint` and `dev test`.

`dev clippy` is the lint gate: it lints every target with warnings denied, for Linux and the Mac from either, on the pinned toolchain. A plain `cargo clippy` passes what it fails.
