# The pitch: open the picker, type three characters, land in the session.
# Run from the repo root: vhs docs/media/hero.tape

Output docs/media/hero.gif

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 20
Set Framerate 24

# A few sessions to pick from. Detached, so this client stays the only one:
# a zellij session takes the size of its smallest attached client.
Hide
Type "zellij -s dotfiles --detached" Enter
Type "zellij -s despesas --detached" Enter
Type "zellij attach --create luneta" Enter
Sleep 3s
Show

Sleep 1s

# Open the picker.
Ctrl+j
Sleep 2s

# Move through the list. The preview follows the highlight, which is the point.
Down@600ms 3
Sleep 2s

# Filter, then attach.
Type@200ms "dot"
Sleep 2s
Enter
Sleep 3s

Hide
Type "exit" Enter
Sleep 500ms
Type "zellij delete-all-sessions --yes --force" Enter
Sleep 1s
