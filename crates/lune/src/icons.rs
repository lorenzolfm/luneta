use std::iter::Peekable;
use std::str::SplitWhitespace;

use luned::api::Harness;

pub const WIDTH: usize = 1;

pub const BROWSER: &str = "\u{f0ac}";

pub use lune_protocol::shell::SHELLS;

const TERMINAL: &str = "\u{e795}";

const NEOVIM: &str = "\u{e6ae}";

const CLAUDE: &str = "\u{ec82}";

const PI: &str = "\u{f03ff}";

const AGY: &str = "\u{f0674}";

const PYTHON: &str = "\u{e73c}";

pub const GIT: &str = "\u{e702}";

const ICONS: [(&str, &str); 13] = [
    ("claude", CLAUDE),
    ("pi", PI),
    ("agy", AGY),
    ("nvim", NEOVIM),
    ("vim", NEOVIM),
    ("cargo", "\u{e7a8}"),
    ("python", PYTHON),
    ("python3", PYTHON),
    ("docker", "\u{e7b0}"),
    ("ssh", "\u{e8b1}"),
    ("git", GIT),
    ("lazygit", GIT),
    ("htop", "\u{eb31}"),
];

pub const fn harness(harness: lune_domain::agent::Harness) -> &'static str {
    match harness {
        lune_domain::agent::Harness::ClaudeCode => CLAUDE,
        lune_domain::agent::Harness::Pi => PI,
        lune_domain::agent::Harness::Antigravity => AGY,
    }
}

pub fn agent(harness: Harness) -> Option<&'static str> {
    match harness {
        Harness::ClaudeCode => Some("claude"),
        Harness::Pi => Some("pi"),
        Harness::Antigravity => Some("agy"),
        Harness::Unspecified => None,
    }
}

pub fn of(program: &str) -> Option<&'static str> {
    if SHELLS.contains(&program) {
        return Some(TERMINAL);
    }
    ICONS
        .iter()
        .find(|(name, _)| *name == program)
        .map(|(_, icon)| *icon)
}

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
            }
            "" => return None,
            name => return Some(name),
        }
    }
}

struct Takes {
    letters: &'static str,
    names: &'static [&'static str],
}

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

const ENV_TAKES: Takes = Takes {
    letters: "Cu",
    names: &["chdir", "unset"],
};

impl Takes {
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

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn binary(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_prefix('-').unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

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
            (
                "nix run --impure nixpkgs#python3Packages.ipython -- -i",
                Some("ipython"),
            ),
            ("nix build .#lune", Some("nix")),
            ("nix run .", None),
            ("sudo -i", None),
            ("sudo -iu postgres", None),
            ("-", None),
            ("", None),
        ] {
            assert_eq!(program(command), runs, "{command:?}");
        }
    }

    #[test]
    fn every_icon_takes_the_columns_a_row_keeps_for_one() {
        assert_eq!(BROWSER.width(), WIDTH, "browser");
        for program in SHELLS.into_iter().chain(ICONS.map(|(program, _)| program)) {
            assert_eq!(
                of(program).map(UnicodeWidthStr::width),
                Some(WIDTH),
                "{program}"
            );
        }
    }
}
