# luneta: say what runs in each zellij pane, and how it ended.
if status is-interactive; and set -q ZELLIJ_PANE_ID; and test -t 0
    and test -n "$XDG_RUNTIME_DIR$TMPDIR"
    # A window opened from a pane has the pane's variables, and a terminal of
    # its own: a shell there is not the pane's.
    and not begin
        test "$__luneta_pane" = "$ZELLIJ_SESSION_NAME $ZELLIJ_PANE_ID"
        and test "$__luneta_pane_tty" != (tty)
    end
    set -gx __luneta_pane "$ZELLIJ_SESSION_NAME $ZELLIJ_PANE_ID"
    set -gx __luneta_pane_tty (tty)
    set -l root $XDG_RUNTIME_DIR
    test -n "$root"; or set root $TMPDIR
    set -g __luneta_root $root/luneta
    set -g __luneta_dir $__luneta_root/commands/$ZELLIJ_SESSION_NAME
    # The shell's own file, for its pane and when it started.
    set -q __luneta_file; or set -g __luneta_file $__luneta_dir/$ZELLIJ_PANE_ID.(date +%s)

    function __luneta_say
        # Yours alone, and no link to another: what runs says what you type.
        test -d $__luneta_root; or mkdir -m 700 $__luneta_root
        test -O $__luneta_root; and not test -L $__luneta_root; or return
        test -d $__luneta_dir; or mkdir -p $__luneta_dir; or return
        # Moved into place whole, as the sidebar reads it.
        printf '%s\n' "$argv" >$__luneta_file.new; and mv -f $__luneta_file.new $__luneta_file
    end

    function __luneta_going --on-event fish_preexec
        set -g __luneta_jobs (jobs --group)
        set -g __luneta_command (string replace -a \n ' ' -- $argv[1])
        # `exec` gives the pane to another program, and nothing would say how
        # this ended.
        if string match -qr '(^|[;&|]|\b(and|or|not))\s*exec(\s|$)' -- $argv[1]
            rm -f $__luneta_file
            return
        end
        # `fg` brings back the last job, which is what runs.
        if string match -qr '^\s*fg\s*$' -- $argv[1]
            set -l job (jobs --last | string split \t)
            if set -q job[1]
                set __luneta_command $job[-1]
                set __luneta_jobs (string match -v -- $job[2] $__luneta_jobs)
            end
        end
        __luneta_say going (date +%s) $__luneta_pane_tty $__luneta_command
    end

    function __luneta_ended --on-event fish_postexec
        set -l s $status
        # `fg` says 0 however the job it brought back ended.
        string match -qr '^\s*fg(\s|$)' -- $argv[1]; and set s fg
        # `Ctrl z` stops the job, and gives the prompt back with a status of 0.
        set -l job (jobs --last --group)
        if set -q job[1]; and not contains -- $job $__luneta_jobs
            and ps -o stat= -p $job | string match -q 'T*'
            __luneta_say stopped (date +%s) $__luneta_command
        else
            __luneta_say ended (date +%s) $s $CMD_DURATION $__luneta_command
        end
    end

    function __luneta_gone --on-event fish_exit
        rm -f $__luneta_file
    end
end
