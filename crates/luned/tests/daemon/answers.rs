use std::fs;
use std::path::Path;
use std::process::Stdio;

use lune_waits::{eventually, eventually_showing};
use luned::api::{self, answer_request, AnswerRequest, WaitingRequest, WaitingResponse};
use luned::mux::own::api as mux_api;
use tonic::Code;

use crate::host::{Running, PATIENT};
use crate::own::{built_server, Owned};

const SCREEN: &str = r#"stty raw -echo
printf "$1"
dd bs=1 count="$2" 2>/dev/null > "$HOME/pressed.raw"
printf "$3"
dd bs=1 count="$4" 2>/dev/null >> "$HOME/pressed.raw"
od -An -tx1 "$HOME/pressed.raw" | tr -s ' \n' ' ' > "$HOME/pressed.draft"
mv "$HOME/pressed.draft" "$HOME/pressed"
sleep 600
"#;

const MENU: &str = r" Do you want to proceed?\r\n \342\235\257 1. Yes\r\n   2. No\r\n\r\n Esc to cancel\r\n";

const MOVED: &str =
    r"\033[H\033[2J Do you want to proceed?\r\n   1. Yes\r\n \342\235\257 2. No\r\n\r\n Esc to cancel\r\n";

const PROMPT: &str = r" What next?\r\n";

struct Shown<'a> {
    first: &'a str,
    presses: usize,
    then: &'a str,
    presses_then: usize,
}

impl Shown<'_> {
    fn still(first: &str, presses: usize) -> Shown<'_> {
        Shown { first, presses, then: "", presses_then: 0 }
    }
}

struct Menu {
    owned: Owned,
    _mux: Running,
}

impl Menu {
    fn new() -> Menu {
        let owned = Owned::new();
        let program = built_server();
        let mut mux = owned.host.vars().command(program.to_str().unwrap());
        mux.arg(owned.host.folder());
        let mux = Running::spawn(mux.stdin(Stdio::null()));
        let socket = owned.socket();
        eventually("lune-mux listens", || socket.exists().then_some(()));
        Menu { owned, _mux: mux }
    }

    fn opened(&self, shown: &Shown<'_>) -> api::TerminalKey {
        let script = Path::new(&self.owned.home()).join("screen.sh");
        fs::write(&script, SCREEN).unwrap();
        let (presses, presses_then) = (shown.presses.to_string(), shown.presses_then.to_string());
        let argv = [script.to_str().unwrap(), shown.first, &presses, shown.then, &presses_then];
        let argv = ["sh"].into_iter().chain(argv).map(str::to_owned).collect();
        let mux_api::TerminalKey { id, made_at } = self.owned.new_terminal_running(b"menu", argv);
        api::TerminalKey { id, made_at }
    }

    fn waiting(&self, terminal: &api::TerminalKey) -> Result<WaitingResponse, tonic::Status> {
        let folder = luned::folder::open(&self.owned.host.folder()).unwrap().unwrap();
        let request = WaitingRequest { on: None, terminal: Some(*terminal) };
        self.owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon.waiting(request).await.map(tonic::Response::into_inner)
        })
    }

    fn answer(
        &self,
        terminal: &api::TerminalKey,
        answer: answer_request::Answer,
    ) -> Result<(), tonic::Status> {
        let folder = luned::folder::open(&self.owned.host.folder()).unwrap().unwrap();
        let request =
            AnswerRequest { on: None, terminal: Some(*terminal), answer: Some(answer) };
        self.owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon.answer(request).await.map(drop)
        })
    }

    fn pressed(&self) -> String {
        let pressed = Path::new(&self.owned.home()).join("pressed");
        eventually("the menu heard its keys", || fs::read_to_string(&pressed).ok())
            .trim()
            .to_owned()
    }

    fn shown(&self, terminal: &api::TerminalKey, text: &str) -> WaitingResponse {
        eventually_showing("the terminal shows its screen", || {
            let waiting = self.waiting(terminal).map_err(|status| status.message().to_owned())?;
            if waiting.screen.contains(text) { Ok(waiting) } else { Err(waiting.screen) }
        })
    }
}

fn choice(index: u32, label: &str) -> answer_request::Answer {
    answer_request::Answer::Choice(answer_request::Choice { index, label: label.to_owned() })
}

fn text(text: &str) -> answer_request::Answer {
    answer_request::Answer::Text(text.to_owned())
}

#[test]
fn a_waiting_terminal_shows_its_screen_and_choices() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 2));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    assert_eq!(waiting.choices, ["Yes", "No"]);
    assert_eq!(waiting.chosen, Some(0));
    assert!(waiting.screen.contains("Do you want to proceed?"), "{}", waiting.screen);
    assert_eq!(waiting.said, None);
}

#[test]
fn a_terminal_with_no_menu_shows_no_choices() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(PROMPT, 2));
    let waiting = menu.shown(&terminal, "What next?");
    assert!(waiting.choices.is_empty(), "{:?}", waiting.choices);
    assert_eq!(waiting.chosen, None);
}

#[test]
fn a_choice_moves_the_cursor_to_it_and_presses_enter_once_it_is_there() {
    let menu = Menu::new();
    let shown = Shown { first: MENU, presses: 3, then: MOVED, presses_then: 1 };
    let terminal = menu.opened(&shown);
    menu.shown(&terminal, "Esc to cancel");
    menu.answer(&terminal, choice(1, "No")).unwrap();
    assert_eq!(menu.pressed(), "1b 5b 42 0d");
}

#[test]
fn a_choice_the_cursor_does_not_reach_is_not_entered() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 3));
    menu.shown(&terminal, "Esc to cancel");
    let refused = menu.answer(&terminal, choice(1, "No")).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    let unreached = r#"the cursor did not reach choice 2, "No", so Enter was not pressed"#;
    assert_eq!(refused.message(), unreached);
    assert_eq!(menu.pressed(), "1b 5b 42");
}

#[test]
fn a_choice_the_screen_no_longer_shows_is_refused() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 4));
    menu.shown(&terminal, "Esc to cancel");
    let refused = menu.answer(&terminal, choice(1, "Maybe")).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert_eq!(refused.message(), r#"choice 2 is now "No", not "Maybe""#);
}

#[test]
fn a_text_answer_is_typed_and_entered() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(PROMPT, 6));
    menu.shown(&terminal, "What next?");
    menu.answer(&terminal, text("hello")).unwrap();
    assert_eq!(menu.pressed(), "68 65 6c 6c 6f 0d");
}

#[test]
fn a_text_answer_is_refused_while_the_screen_shows_choices() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 6));
    menu.shown(&terminal, "Esc to cancel");
    let refused = menu.answer(&terminal, text("hello")).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert_eq!(refused.message(), "the screen shows 2 choices now: answer with one of them");
}

#[test]
fn escape_is_pressed_alone() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 1));
    menu.shown(&terminal, "Esc to cancel");
    menu.answer(&terminal, answer_request::Answer::Escape(())).unwrap();
    assert_eq!(menu.pressed(), "1b");
}
