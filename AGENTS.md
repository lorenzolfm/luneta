# luneta

Run cargo inside `nix develop`: the host's cargo can't build `alsa-sys`.

`dev` comes with that shell. Outside it, `nix run .#dev -- <command>`. `dev help` lists every command.

Before a commit, run `dev fmt`, `dev check`, `dev clippy`, `dev lint` and `dev test`. After changing code behind `cfg(target_os = "macos")`, run `dev clippy-mac` too.

`dev clippy` is the lint gate: it lints every target with warnings denied, on the pinned toolchain. A plain `cargo clippy` passes what it fails.
