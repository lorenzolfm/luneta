function __luneta_folder
    if string match -q '/*' -- "$XDG_RUNTIME_DIR"
        echo $XDG_RUNTIME_DIR/luneta
    else if test (uname) = Darwin
        set -l root (/usr/bin/getconf DARWIN_USER_TEMP_DIR | string trim -r -c /)
        string match -q '/*' -- "$root"; and echo $root/luneta
    else
        echo /tmp/luneta-(id -u)
    end
end

if status is-interactive; and set -q ZELLIJ_PANE_ID; and test -t 0
    and not begin
        test "$__luneta_pane" = "$ZELLIJ_SESSION_NAME $ZELLIJ_PANE_ID"
        and test "$__luneta_pane_tty" != (tty)
    end
    set -gx __luneta_pane "$ZELLIJ_SESSION_NAME $ZELLIJ_PANE_ID"
    set -gx __luneta_pane_tty (tty)
    set -g __luneta_root (__luneta_folder)
    set -g __luneta_dir $__luneta_root/commands/$ZELLIJ_SESSION_NAME
    set -q __luneta_file; or set -g __luneta_file $__luneta_dir/$ZELLIJ_PANE_ID.(date +%s)

    function __luneta_say
        test -n "$__luneta_root"; or return
        test -d $__luneta_root; or mkdir -m 700 $__luneta_root
        test -O $__luneta_root; and not test -L $__luneta_root; or return
        test -d $__luneta_dir; or mkdir -p $__luneta_dir; or return
        printf '%s\n' "$argv" >$__luneta_file.new; and mv -f $__luneta_file.new $__luneta_file
    end

    function __luneta_going --on-event fish_preexec
        set -g __luneta_jobs (jobs --group)
        set -g __luneta_command (string replace -a \n ' ' -- $argv[1])
        if string match -qr '(^|[;&|]|\b(and|or|not))\s*exec(\s|$)' -- $argv[1]
            rm -f $__luneta_file
            return
        end
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
        set -q __luneta_command; or return
        string match -qr '^\s*fg(\s|$)' -- $argv[1]; and set s fg
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

    test -e $__luneta_file; or __luneta_say ready
end
