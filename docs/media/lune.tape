Output docs/media/lune.gif

Require lune
Require git
Require mktemp
Require env
Require fish

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 16
Set Width 1400
Set Height 760
Set Padding 20

Hide
Type@1ms `keep=(PATH="${PATH:-}" TERM="${TERM:-xterm-256color}" COLORTERM="${COLORTERM:-truecolor}"); for name in LANG LC_ALL LC_CTYPE LOCALE_ARCHIVE TERMINFO TERMINFO_DIRS TMPDIR TZ TZDIR; do if [ -n "${!name:-}" ]; then keep+=("$name=${!name}"); fi; done; exec env -i "${keep[@]}" bash --noprofile --norc` Enter
Type@1ms `set -euo pipefail; printf 'demo-%s\n' env` Enter
Wait+Screen@5s /demo-env/
Type@1ms `repo="$PWD"` Enter
Type@1ms `git -C "$repo" show-ref --verify --quiet refs/heads/main` Enter
Type@1ms `if [ "$(uname)" = Darwin ]; then login_shell="$(dscl . -read "/Users/$(id -un)" UserShell)"; login_shell="${login_shell#UserShell: }"; else login_shell="$(getent passwd "$(id -u)")"; login_shell="${login_shell##*:}"; fi` Enter
Type@1ms `[[ $login_shell == /* ]]; [ -f "$login_shell" ]; [ -x "$login_shell" ]; [ "${login_shell##*/}" = fish ]` Enter
Type@1ms `demo=""; cleaned=0` Enter
Type@1ms `stop_demo() {` Enter
Type@1ms `  local attempt plan status out` Enter
Type@1ms `  local cmd=(env -u XDG_STATE_HOME -u LUNE_MUX_TERMINAL -u LUNE_MUX_TERMINAL_MADE -u LUNE_TURN -u LUNE_BEFORE HOME="$demo/home" XDG_RUNTIME_DIR="$demo/run" lune terminate)` Enter
Type@1ms `  for attempt in 1 2 3; do` Enter
Type@1ms `    status=0` Enter
Type@1ms `    out="$("${cmd[@]}" 2>&1 >/dev/null)" || status=$?` Enter
Type@1ms `    [ "$status" -eq 3 ] || return 1` Enter
Type@1ms `    [[ $out =~ --yes\ ([0-9a-f]{16}) ]] || return 1` Enter
Type@1ms `    plan="${BASH_REMATCH[1]}"` Enter
Type@1ms `    status=0` Enter
Type@1ms `    "${cmd[@]}" --yes "$plan" >/dev/null || status=$?` Enter
Type@1ms `    if [ "$status" -ne 3 ]; then return "$status"; fi` Enter
Type@1ms `  done` Enter
Type@1ms `  return 1` Enter
Type@1ms `}` Enter
Type@1ms `cleanup() {` Enter
Type@1ms `  if [ "$cleaned" -ne 0 ]; then return 0; fi` Enter
Type@1ms `  cleaned=1` Enter
Type@1ms `  trap - EXIT HUP INT TERM` Enter
Type@1ms `  local rc=0` Enter
Type@1ms `  if [ -n "$demo" ] && [ -d "$demo/run/lune" ]; then stop_demo || rc=1; fi` Enter
Type@1ms `  if [ -n "$demo" ] && [ -d "$demo" ]; then rm -rf "$demo" || rc=1; [ ! -e "$demo" ] || rc=1; fi` Enter
Type@1ms `  return "$rc"` Enter
Type@1ms `}` Enter
Type@1ms `trap cleanup EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM` Enter
Type@1ms `demo="$(mktemp -d)"` Enter
Type@1ms `[ -n "$demo" ]; [ -d "$demo" ]` Enter
Type@1ms `demo="$(cd -- "$demo" && pwd -P)"` Enter
Type@1ms `[[ $demo == /* ]]; [ -d "$demo" ]` Enter
Type@1ms `export HOME="$demo/home" XDG_RUNTIME_DIR="$demo/run"` Enter
Type@1ms `mkdir -p -m 700 "$XDG_RUNTIME_DIR"` Enter
Type@1ms `mkdir -p "$HOME/.config/fish" "$HOME/code/notes" "$HOME/code/dotfiles"` Enter
Type@1ms `printf 'set -g fish_greeting\nfunction fish_prompt; printf "%%s> " (prompt_pwd); end\n' > "$HOME/.config/fish/config.fish"` Enter
Type@1ms `git clone --quiet --branch main "$repo" "$HOME/code/lune"` Enter
Type@1ms `git -C "$HOME/code/lune" remote set-url origin https://github.com/lorenzolfm/lune` Enter
Type@1ms `for project in lune notes dotfiles; do lune start "$HOME/code/$project"; done; printf '\033[3J\033[H\033[2Jdemo-%s\n' ready` Enter
Wait+Screen@20s /demo-ready/
Type@1ms `cd "$HOME/code/lune"; printf '\033[3J\033[H\033[2J'; SSH_TTY=1 lune; cleanup; printf 'demo-%s\n' done` Enter
Wait+Screen@15s /3 projects/
Wait+Screen@15s /~\/c\/lune> /
Type "git log --oneline --no-decorate -12" Enter
Wait+Screen@10s /[0-9a-f]{7} /
Wait+Screen@10s /~\/c\/lune>  /
Show
Sleep 500ms
Screenshot docs/media/lune.png
Sleep 200ms
Hide
Alt+a
Type "q"
Wait+Screen@15s /demo-done/
