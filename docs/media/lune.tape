Output docs/media/lune.gif

Require lune
Require git
Require mktemp
Require ps
Require awk

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 16
Set Width 1400
Set Height 760
Set Padding 20

Hide
Type@1ms `set -euo pipefail` Enter
Type@1ms `repo="$PWD"` Enter
Type@1ms `git -C "$repo" rev-parse --verify --quiet main >/dev/null` Enter
Type@1ms `demo="$(mktemp -d)"` Enter
Type@1ms `[ -n "$demo" ]` Enter
Type@1ms `[ -d "$demo" ]` Enter
Type@1ms `wait_gone() {` Enter
Type@1ms `  local tries="$1" i pid left` Enter
Type@1ms `  shift` Enter
Type@1ms `  [[ $tries =~ ^[1-9][0-9]*$ ]]` Enter
Type@1ms `  [ "$#" -gt 0 ]` Enter
Type@1ms `  for ((i = 0; i < tries; i++)); do` Enter
Type@1ms `    left=()` Enter
Type@1ms `    for pid in "$@"; do if kill -0 "$pid" 2>/dev/null; then left+=("$pid"); fi; done` Enter
Type@1ms `    [ "${#left[@]}" -gt 0 ] || return 0` Enter
Type@1ms `    sleep 0.1` Enter
Type@1ms `  done` Enter
Type@1ms `  return 1` Enter
Type@1ms `}` Enter
Type@1ms `stop_pids() {` Enter
Type@1ms `  [ "$#" -gt 0 ] || return 0` Enter
Type@1ms `  local pid` Enter
Type@1ms `  for pid in "$@"; do [[ $pid =~ ^[1-9][0-9]*$ ]]; done` Enter
Type@1ms `  kill -TERM "$@" 2>/dev/null || true` Enter
Type@1ms `  if wait_gone 50 "$@"; then return 0; fi` Enter
Type@1ms `  kill -KILL "$@" 2>/dev/null || true` Enter
Type@1ms `  wait_gone 20 "$@"` Enter
Type@1ms `}` Enter
Type@1ms `cleanup() {` Enter
Type@1ms `  trap - EXIT HUP` Enter
Type@1ms `  local folder="$demo/run/lune" pids=() info_out mux_out mux_pid` Enter
Type@1ms `  if [ -d "$folder" ]; then` Enter
Type@1ms `    if info_out="$(XDG_RUNTIME_DIR="$demo/run" lune info 2>/dev/null)"; then` Enter
Type@1ms `      [[ $info_out =~ ^luned\ [^[:space:]]+\ \(pid\ ([1-9][0-9]*)\) ]]` Enter
Type@1ms `      pids+=("${BASH_REMATCH[1]}")` Enter
Type@1ms `    fi` Enter
Type@1ms `    mux_out="$(ps -ww -u "$(id -u)" -o pid= -o args= | awk -v folder="$folder" '$2 ~ /(^|\/)lune-mux$/ && NF == 3 && $3 == folder { print $1 }')"` Enter
Type@1ms `    for mux_pid in $mux_out; do` Enter
Type@1ms `      [[ $mux_pid =~ ^[1-9][0-9]*$ ]]` Enter
Type@1ms `      pids+=("$mux_pid")` Enter
Type@1ms `    done` Enter
Type@1ms `  fi` Enter
Type@1ms `  if [ "${#pids[@]}" -gt 0 ]; then stop_pids "${pids[@]}"; fi` Enter
Type@1ms `  if [ -n "${demo:-}" ] && [ -d "$demo" ]; then` Enter
Type@1ms `    rm -rf "$demo"` Enter
Type@1ms `    [ ! -e "$demo" ]` Enter
Type@1ms `  fi` Enter
Type@1ms `}` Enter
Type@1ms `trap cleanup EXIT; trap 'exit 129' HUP` Enter
Type@1ms `export HOME="$demo/home" XDG_RUNTIME_DIR="$demo/run"` Enter
Type@1ms `unset XDG_STATE_HOME XDG_DATA_HOME XDG_CONFIG_HOME LUNE_MUX_TERMINAL LUNE_MUX_TERMINAL_MADE` Enter
Type@1ms `mkdir -p -m 700 "$XDG_RUNTIME_DIR"` Enter
Type@1ms `mkdir -p "$HOME/.config/fish" "$HOME/code/notes" "$HOME/code/dotfiles"` Enter
Type@1ms `printf 'set -g fish_greeting\nfunction fish_prompt; printf "%%s> " (prompt_pwd); end\n' > "$HOME/.config/fish/config.fish"` Enter
Type@1ms `git clone --quiet --branch main "$repo" "$HOME/code/lune"` Enter
Type@1ms `git -C "$HOME/code/lune" remote set-url origin https://github.com/lorenzolfm/lune` Enter
Type@1ms `for project in lune notes dotfiles; do lune start "$HOME/code/$project"; done && printf '\033[3J\033[H\033[2Jdemo-%s\n' ready` Enter
Wait+Screen@20s /demo-ready/
Type@1ms `cd "$HOME/code/lune" && printf '\033[3J\033[H\033[2J' && lune && cleanup && printf 'demo-%s\n' done` Enter
Wait+Screen@15s /3 projects/
Wait+Screen@15s /~\/c\/lune> /
Type "git log --oneline --no-decorate -12" Enter
Wait+Screen@10s /[0-9a-f]{7} /
Wait+Screen@10s /~\/c\/lune>  /
Show
Sleep 500ms
Screenshot docs/media/lune.png
Hide
Alt+a
Type "q"
Wait+Screen@15s /demo-done/
