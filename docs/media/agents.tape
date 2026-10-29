# The agent screen: which one stopped and needs me.
# Needs `luneta` and `lunetad` on the PATH and at least one Claude Code agent running.
# Run from the repo root: vhs docs/media/agents.tape

Output docs/media/agents.gif

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 20
Set Framerate 24

Hide
Type "luneta luneta" Enter
Sleep 3s
Show

Sleep 1s

Ctrl+j
Sleep 1500ms

# Tab to the agents. The search term follows across, so this is the same question
# asked of the next list.
Tab
Sleep 2500ms

# A waiting agent's preview shows the question it is stuck on.
Down@600ms 2
Sleep 2500ms
Up@600ms 2
Sleep 1500ms

Enter
Sleep 3s

# Detach zellij, which ends luneta.
Hide
Ctrl+o
Type "d"
Sleep 1s
