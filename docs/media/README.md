# README media

The README ships an ASCII rendering of each screen with an `<!-- MEDIA SLOT -->`
comment above it naming the GIF that replaces it.

The GIFs are recorded by [vhs](https://github.com/charmbracelet/vhs), which drives
a real terminal from a script. The tapes here press real keys against the
installed `.wasm`.

```sh
nix shell nixpkgs#vhs        # or: go install github.com/charmbracelet/vhs@latest
make media                   # writes hero.gif, agents.gif, dirs.gif here
```

Or one at a time: `vhs docs/media/hero.tape`.

They assume `zoxide`, `eza` and `claude-ps` on the `PATH`, a populated zoxide
database, at least one Claude Code agent running, and a Nerd Font as the terminal
font (`Set FontFamily` in each tape).

Then in the README, replace the fenced block under a slot with the `<img>` line
the comment gives. Keep each GIF under a megabyte — `Set Framerate` and a shorter
tape are the two knobs.
