//! The icon on a pane's row in the sidebar, for the program that runs in the
//! pane: Claude's for an agent, a terminal for a shell, neovim's for nvim.
//!
//! The program is the agent in the pane, or else what the daemon says the
//! pane's terminal runs in the foreground: the process group in front, or the
//! shell itself at its prompt. The icons are the plugin's own, from its
//! `src/icons.rs`, and Nerd Fonts', which only a Nerd Font draws. The plugin's
//! configuration changes them with `icons`; luneta has no configuration yet.

use std::iter::Peekable;
use std::str::SplitWhitespace;

/// The program an agent runs, by its binary's name.
pub const AGENT: &str = "claude";

/// How many columns an icon takes: one, as each of these does in a font whose
/// name ends in "Mono". Each pane's row keeps that many for one, whether its
/// program has one or not.
pub const WIDTH: usize = 1;

/// `dev-terminal`, for a shell.
const TERMINAL: &str = "\u{e795}";

/// `custom-neovim`, for vim as well.
const NEOVIM: &str = "\u{e6ae}";

/// `dev-python`.
const PYTHON: &str = "\u{e73c}";

/// `dev-git`.
const GIT: &str = "\u{e702}";

/// The icons, by the name of the binary each is for, as Nerd Fonts 3.5
/// numbers them. Claude's came with Nerd Fonts 3.5.0, and ssh's with 3.3.0: an
/// older font has neither, and the terminal draws a box or nothing for them.
const ICONS: [(&str, &str); 16] = [
    (AGENT, "\u{ec82}"), // `cod-claude`
    ("bash", TERMINAL),
    ("fish", TERMINAL),
    ("nu", TERMINAL),
    ("sh", TERMINAL),
    ("zsh", TERMINAL),
    ("nvim", NEOVIM),
    ("vim", NEOVIM),
    ("cargo", "\u{e7a8}"), // `dev-rust`
    ("python", PYTHON),
    ("python3", PYTHON),
    ("docker", "\u{e7b0}"), // `dev-docker`
    ("ssh", "\u{e8b1}"),    // `dev-ssh`
    ("git", GIT),
    ("lazygit", GIT),
    ("htop", "\u{eb31}"), // `cod-pulse`
];

/// The icon of `program`, by its binary's name, if it has one.
pub fn of(program: &str) -> Option<&'static str> {
    ICONS.iter().find(|(name, _)| *name == program).map(|(_, icon)| *icon)
}

/// The name of the binary a command line runs, as zellij spells a command:
/// its path, then each argument after a space. A login shell's leading `-`
/// goes, and `sudo`, `env` and `nix run` give way to what they run: `nix
/// run`'s is the last part of the attribute it is given, as `htop` of
/// `nixpkgs#htop`, and nothing a binary goes by for a flake's default. It
/// reads no further than the binary, however many arguments follow it.
pub fn program(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace().peekable();
    loop {
        let word = words.next()?;
        if is_assignment(word) {
            continue;
        }
        match binary(word) {
            "sudo" => past_options(&mut words, &SUDO_TAKES),
            "env" => past_options(&mut words, &ENV_TAKES),
            "nix" if words.next_if_eq(&"run").is_some() => {
                let installable = words.find(|word| !word.starts_with('-'))?;
                let (_, attribute) = installable.split_once('#')?;
                return attribute.rsplit('.').next().filter(|name| !name.is_empty());
            },
            "" => return None,
            name => return Some(name),
        }
    }
}

/// The options of a command that take a value, by their letters after a `-`
/// and their names after `--`.
struct Takes {
    letters: &'static str,
    names: &'static [&'static str],
}

/// The options of `sudo` that take a value.
const SUDO_TAKES: Takes = Takes {
    letters: "CDRTUghprtu",
    names: &[
        "chdir",
        "chroot",
        "close-from",
        "command-timeout",
        "group",
        "host",
        "other-user",
        "prompt",
        "role",
        "type",
        "user",
    ],
};

/// The options of `env` that take a value.
const ENV_TAKES: Takes = Takes { letters: "Cu", names: &["chdir", "unset"] };

impl Takes {
    /// Whether `option` takes the word after it as its value: a name that
    /// takes one, with none after an `=`, or a run of letters whose first
    /// that takes one is its last, as the `u` of `-Hu root`. One before the
    /// last takes the rest of the run, as the `u` of `-uroot`.
    fn next_word(&self, option: &str) -> bool {
        if let Some(name) = option.strip_prefix("--") {
            return self.names.contains(&name);
        }
        let letters = option.strip_prefix('-').unwrap_or(option);
        letters
            .find(|letter: char| self.letters.contains(letter))
            .is_some_and(|at| at + 1 == letters.len())
    }
}

/// Takes the options at the front of `words` off it, the value of each that
/// takes the word after it, and a `--` that ends them.
fn past_options(words: &mut Peekable<SplitWhitespace>, takes: &Takes) {
    while let Some(option) = words.next_if(|word| word.starts_with('-')) {
        if option == "--" {
            return;
        }
        if takes.next_word(option) {
            words.next();
        }
    }
}

/// Whether `word` sets a variable for the command after it, as `FOO=1`.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The name of the binary at `path`, without the `-` a login shell leads its
/// own with.
fn binary(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_prefix('-').unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    /// As zellij 0.45 spelled them, run from fish on NixOS, and what they run.
    #[test]
    fn a_command_line_runs_the_binary_it_names_past_sudo_env_and_nix_run() {
        let nvim = "/nix/store/q2rpnq6f54cba9mx1q159i1qw994z52k-neovim-unwrapped-0.12.6/bin/nvim \
                    --cmd lua vim.g.loaded_node_provider=0 /etc/hosts";
        let sudo = "/nix/store/fdsygnh4wvr7g860ba3cwlcksn8v1pc5-sudo-1.9.17p2/bin/sudo -k nvim x";
        for (command, runs) in [
            ("fish", Some("fish")),
            ("/run/current-system/sw/bin/fish", Some("fish")),
            ("-bash", Some("bash")),
            (nvim, Some("nvim")),
            ("python3 -q", Some("python3")),
            ("ssh -o ConnectTimeout=60 10.255.255.1", Some("ssh")),
            (sudo, Some("nvim")),
            ("sudo -u postgres psql", Some("psql")),
            ("sudo -Hu postgres psql", Some("psql")),
            ("sudo -upostgres -E psql", Some("psql")),
            ("sudo --user postgres -- psql", Some("psql")),
            ("sudo --user=postgres psql", Some("psql")),
            ("sudo FOO=1 env -i -C /w PATH=/bin htop", Some("htop")),
            ("nix run nixpkgs#htop", Some("htop")),
            ("nix run --impure nixpkgs#python3Packages.ipython -- -i", Some("ipython")),
            ("nix build .#luneta", Some("nix")),
            // A flake's default, a shell `sudo` starts, and nothing at all.
            ("nix run .", None),
            ("sudo -i", None),
            ("sudo -iu postgres", None),
            ("-", None),
            ("", None),
        ] {
            assert_eq!(program(command), runs, "{command:?}");
        }
    }

    /// The sidebar keeps a column of `WIDTH` for an icon, so that every label
    /// starts at the same column: one that took more would push its label on.
    #[test]
    fn every_icon_takes_the_columns_a_row_keeps_for_one() {
        for (program, icon) in ICONS {
            assert_eq!(icon.width(), WIDTH, "{program}");
        }
    }
}
