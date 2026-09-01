# The directory screen: zoxide frecency, eza preview, and a name nothing holds.
# Run from the repo root: vhs docs/media/dirs.tape

Output docs/media/dirs.gif

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 20
Set Framerate 24

Hide
Type "zellij attach --create luneta" Enter
Sleep 3s
Show

Sleep 1s

Ctrl+j
Sleep 1500ms

# Two tabs to the directories.
Tab
Sleep 1s
Tab
Sleep 2500ms

# The preview is eza's own output: icons, colours, directories first.
Down@600ms 3
Sleep 2500ms

# Type a path fragment. The filter searches the path, because the path is what you
# would type at `z`.
Type@200ms "proj"
Sleep 2500ms

# The prompt shows the session name you will get, postfix and all.
Enter
Sleep 3s

Hide
Type "exit" Enter
Sleep 500ms
Type "zellij delete-all-sessions --yes --force" Enter
Sleep 1s
