# README media

The README shows a screenshot of each screen — `sessions.png`, `agents.png`,
`dirs.png` — with an `<!-- MEDIA SLOT -->` comment above it naming the GIF that
can replace it.

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

Then in the README, point the `<img>` under a slot at the GIF instead of the PNG.
Keep each GIF under a megabyte — `Set Framerate` and a shorter tape are the two
knobs.
