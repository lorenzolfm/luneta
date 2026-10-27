//! The icon on a pane's row in the sidebar, for the program that runs in the
//! pane: Claude's for an agent, a terminal for a shell, neovim's for nvim.
//!
//! The program is the agent the daemon finds in the pane, or else what zellij
//! says the pane's terminal runs in the foreground: the process group in
//! front, or the shell itself at its prompt. The icons are Nerd Fonts', which
//! only a Nerd Font draws, so a configuration can turn them off, and give a
//! program an icon of its own.

use std::collections::BTreeMap;
use std::iter::Peekable;
use std::str::SplitWhitespace;

use kdl::{KdlDocument, KdlNode};
use unicode_width::UnicodeWidthStr;

/// What holds the icons in a configuration: `false` for none, or a node whose
/// children each give a program an icon, which zellij hands over as their KDL.
pub const KEY: &str = "icons";

/// The program an agent the daemon finds runs, by its binary's name.
pub const AGENT: &str = "claude";

/// `dev-terminal`, for a shell.
const TERMINAL: &str = "\u{e795}";

/// `custom-neovim`, for vim as well.
const NEOVIM: &str = "\u{e6ae}";

/// `dev-python`.
const PYTHON: &str = "\u{e73c}";

/// `dev-git`.
const GIT: &str = "\u{e702}";

/// luneta's icons, by the name of the binary each is for, as Nerd Fonts 3.5
/// numbers them. Claude's came with Nerd Fonts 3.5.0, and ssh's with 3.3.0: an
/// older font has neither, and the terminal draws a box or nothing for them.
const DEFAULTS: [(&str, &str); 16] = [
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

/// The icon of each program that has one, by its binary's name.
pub struct Icons {
    by_name: BTreeMap<String, String>,
    /// How many columns the widest takes: the column each pane's row keeps
    /// for one, whether its program has one or not.
    width: usize,
}

impl Default for Icons {
    /// None, as `icons false` asks: no row keeps a column for one.
    fn default() -> Self {
        Icons::new(BTreeMap::new())
    }
}

impl Icons {
    /// luneta's.
    pub fn luneta() -> Self {
        let by_name =
            DEFAULTS.iter().map(|(name, icon)| ((*name).to_owned(), (*icon).to_owned())).collect();
        Icons::new(by_name)
    }

    fn new(by_name: BTreeMap<String, String>) -> Self {
        let width = by_name.values().map(|icon| icon.width()).max().unwrap_or(0);
        Icons { by_name, width }
    }

    /// The icon of `program`, by its binary's name, if it has one.
    pub fn of(&self, program: &str) -> Option<&str> {
        self.by_name.get(program).map(String::as_str)
    }

    pub fn width(&self) -> usize {
        self.width
    }
}

/// The icons a configuration's `icons` asks for, and what is wrong with them:
/// luneta's when it says nothing, none for `false`, and luneta's with yours
/// over them for a node with children, each a program by its binary's name
/// and its icon. An icon of `""` leaves its program none. Icons luneta cannot
/// read leave luneta's in their place.
pub fn configured(value: Option<&str>) -> (Icons, Option<String>) {
    match read(value) {
        Ok(icons) => (icons, None),
        Err(wrong) => (Icons::luneta(), Some(wrong)),
    }
}

fn read(value: Option<&str>) -> Result<Icons, String> {
    let text = match value.map(str::trim) {
        None | Some("true") => return Ok(Icons::luneta()),
        Some("false") => return Ok(Icons::default()),
        Some(text) => text,
    };
    let document: KdlDocument = text.parse().map_err(|error| format!("icons: {error}"))?;
    let mut by_name = Icons::luneta().by_name;
    let mut seen: Vec<&str> = Vec::new();
    for node in document.nodes() {
        let (name, icon) = icon(node)?;
        if seen.contains(&name) {
            return Err(format!("icons: two icons for \"{name}\""));
        }
        seen.push(name);
        match icon {
            Some(icon) => by_name.insert(name.to_owned(), icon.to_owned()),
            None => by_name.remove(name),
        };
    }
    Ok(Icons::new(by_name))
}

/// A child of `icons`: the name of a program's binary, and its icon, the one
/// string it takes, one or two columns wide, or none for `""`.
fn icon(node: &KdlNode) -> Result<(&str, Option<&str>), String> {
    let name = node.name().value();
    let icon = match node.entries() {
        [only] if only.name().is_none() && node.children().is_none() => {
            only.value().as_string().map(str::trim)
        },
        _ => None,
    };
    let Some(icon) = icon else {
        return Err(format!("icons: \"{name}\" takes one string, its icon"));
    };
    if icon.is_empty() {
        return Ok((name, None));
    }
    if icon.chars().any(char::is_control) {
        return Err(format!("icons: the icon of \"{name}\" has a control character"));
    }
    let width = icon.width();
    if !(1..=2).contains(&width) {
        return Err(format!("icons: the icon of \"{name}\" takes {width} columns, not one or two"));
    }
    Ok((name, Some(icon)))
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

    #[test]
    fn a_configuration_puts_its_icons_over_luneta_s_or_turns_them_off() {
        for value in [None, Some("true"), Some(" ")] {
            let (icons, wrong) = configured(value);
            let (claude, nvim) = (icons.of("claude"), icons.of("nvim"));
            assert_eq!((claude, nvim), (Some("\u{ec82}"), Some("\u{e6ae}")), "{value:?}");
            assert_eq!((icons.width(), wrong), (1, None), "{value:?}");
        }
        let (off, wrong) = configured(Some("false"));
        assert_eq!((off.of("claude"), off.width(), wrong), (None, 0, None));

        let (icons, wrong) = configured(Some(r#"nvim "N"; make "M"; cargo "\u{1F980}"; htop """#));
        assert_eq!(wrong, None);
        assert_eq!(icons.of("nvim"), Some("N"));
        assert_eq!(icons.of("make"), Some("M"));
        assert_eq!(icons.of("cargo"), Some("🦀"));
        assert_eq!(icons.of("htop"), None);
        assert_eq!(icons.of("claude"), Some("\u{ec82}"));
        // The crab takes two columns, and so does the column for every icon.
        assert_eq!(icons.width(), 2);
    }

    /// Icons luneta cannot read say what is wrong with them, and leave
    /// luneta's in their place.
    #[test]
    fn icons_luneta_cannot_read_say_so_and_leave_luneta_s() {
        let one_string = r#"icons: "nvim" takes one string, its icon"#;
        for (value, said) in [
            ("nvim", one_string),
            (r#"nvim "a" "b""#, one_string),
            (r#"nvim icon="a""#, one_string),
            (r#"nvim "a" { vim "b"; }"#, one_string),
            ("nvim 1", one_string),
            (r#"nvim "a"; nvim "b""#, r#"icons: two icons for "nvim""#),
            (r#"nvim "\u{1b}[31m""#, r#"icons: the icon of "nvim" has a control character"#),
            // A word for an icon, an escape a raw string leaves as it is, and a
            // character that takes no column.
            (r#"nvim "nvim""#, r#"icons: the icon of "nvim" takes 4 columns, not one or two"#),
            (r#"nvim r"\u{e6ae}""#, r#"icons: the icon of "nvim" takes 8 columns, not one or two"#),
            (r#"nvim "\u{200b}""#, r#"icons: the icon of "nvim" takes 0 columns, not one or two"#),
        ] {
            let (icons, wrong) = configured(Some(value));
            assert_eq!(wrong.as_deref(), Some(said), "{value:?}");
            assert_eq!(icons.of("nvim"), Some("\u{e6ae}"), "{value:?}");
        }
        let (icons, wrong) = configured(Some(r#"nvim ""#));
        assert_eq!(icons.of("nvim"), Some("\u{e6ae}"));
        assert!(wrong.is_some_and(|wrong| wrong.starts_with("icons: ")));
    }
}
