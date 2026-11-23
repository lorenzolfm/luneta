use std::fs;
use std::path::Path;
use std::process::Stdio;

use lune_waits::{eventually, eventually_showing};
use luned::api::{self, answer_request, AnswerRequest, WaitingRequest, WaitingResponse};
use luned::mux::own::api as mux_api;
use tonic::Code;

use crate::host::{Running, PATIENT};
use crate::own::{built_server, Owned};

const MENU: &str = r#"stty raw -echo
printf ' Do you want to proceed?\r\n \342\235\257 1. Yes\r\n   2. No\r\n\r\n Esc to cancel\r\n'
dd bs=1 count="$1" 2>/dev/null | od -An -tx1 | tr -s ' \n' ' ' > "$HOME/pressed.draft"
mv "$HOME/pressed.draft" "$HOME/pressed"
sleep 600
"#;

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

    fn opened(&self, presses: usize) -> api::TerminalKey {
        let script = Path::new(&self.owned.home()).join("menu.sh");
        fs::write(&script, MENU).unwrap();
        let argv = ["sh", script.to_str().unwrap(), "x"].map(str::to_owned);
        let mut argv = argv.to_vec();
        argv[2] = presses.to_string();
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

    fn shown(&self, terminal: &api::TerminalKey) -> WaitingResponse {
        eventually_showing("the menu shows its choices", || {
            let waiting = self.waiting(terminal).map_err(|status| status.message().to_owned())?;
            if waiting.choices.is_empty() { Err(waiting.screen) } else { Ok(waiting) }
        })
    }
}

fn choice(index: u32, label: &str) -> answer_request::Answer {
    answer_request::Answer::Choice(answer_request::Choice { index, label: label.to_owned() })
}

#[test]
fn a_waiting_terminal_shows_its_screen_and_choices() {
    let menu = Menu::new();
    let terminal = menu.opened(2);
    let waiting = menu.shown(&terminal);
    assert_eq!(waiting.choices, ["Yes", "No"]);
    assert_eq!(waiting.chosen, 0);
    assert!(waiting.screen.contains("Do you want to proceed?"), "{}", waiting.screen);
    assert_eq!(waiting.said, None);
}

#[test]
fn a_choice_moves_the_cursor_to_it_and_presses_enter() {
    let menu = Menu::new();
    let terminal = menu.opened(4);
    menu.shown(&terminal);
    menu.answer(&terminal, choice(1, "No")).unwrap();
    assert_eq!(menu.pressed(), "1b 5b 42 0d");
}

#[test]
fn a_choice_the_screen_no_longer_shows_is_refused() {
    let menu = Menu::new();
    let terminal = menu.opened(4);
    menu.shown(&terminal);
    let refused = menu.answer(&terminal, choice(1, "Maybe")).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert_eq!(refused.message(), r#"choice 2 is now "No", not "Maybe""#);
}

#[test]
fn a_text_answer_is_typed_and_entered() {
    let menu = Menu::new();
    let terminal = menu.opened(6);
    menu.shown(&terminal);
    menu.answer(&terminal, answer_request::Answer::Text("hello".to_owned())).unwrap();
    assert_eq!(menu.pressed(), "68 65 6c 6c 6f 0d");
}

#[test]
fn escape_is_pressed_alone() {
    let menu = Menu::new();
    let terminal = menu.opened(1);
    menu.shown(&terminal);
    menu.answer(&terminal, answer_request::Answer::Escape(())).unwrap();
    assert_eq!(menu.pressed(), "1b");
}
