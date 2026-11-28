use std::fs;
use std::path::Path;

use lune_waits::{eventually, eventually_showing};
use luned::api::waiting_response::{Asks, Menu as Choices};
use luned::api::{self, AnswerRequest, WaitingRequest, WaitingResponse, answer_request};
use luned::mux::own::api as mux_api;
use tonic::Code;

use crate::agents::file_of_pid;
use crate::host::PATIENT;
use crate::own::Owned;

const SCREEN: &str = r#"echo $$ > "$HOME/pid.draft" && mv "$HOME/pid.draft" "$HOME/pid"
stty raw -echo
printf "$1"
dd bs=1 count="$2" 2>/dev/null > "$HOME/pressed.raw"
printf "$3"
dd bs=1 count="$4" 2>/dev/null >> "$HOME/pressed.raw"
od -An -tx1 "$HOME/pressed.raw" | tr -s ' \n' ' ' > "$HOME/pressed.draft"
mv "$HOME/pressed.draft" "$HOME/pressed"
sleep 600
"#;

const MENU: &str =
    r" Do you want to proceed?\r\n \342\235\257 1. Yes\r\n   2. No\r\n\r\n Esc to cancel\r\n";

const MOVED: &str = r"\033[H\033[2J Do you want to proceed?\r\n   1. Yes\r\n \342\235\257 2. No\r\n\r\n Esc to cancel\r\n";

const PROMPT: &str = r" What next?\r\n";

const ON_NO: &str =
    r" Do you want to proceed?\r\n   1. Yes\r\n \342\235\257 2. No\r\n\r\n Esc to cancel\r\n";

const ASKED_ANEW: &str = r"\033[H\033[2J Run rm -rf ./work?\r\n \342\235\257 1. Yes\r\n   2. No\r\n\r\n Esc to cancel\r\n";

const SENT: &str = concat!(
    r"\342\235\257 fix the failing\r\n  socket test\r\n\r\n\342\227\217 Done.\r\n\r\n",
    r"\342\224\200\342\224\200\342\224\200\342\224\200\r\n\342\235\257\302\240\r\n",
    r"\342\224\200\342\224\200\342\224\200\342\224\200\r\n  Opus 5.5\r\n",
);

struct Shown<'a> {
    first: &'a str,
    presses: usize,
    then: &'a str,
    presses_then: usize,
}

impl Shown<'_> {
    fn still(first: &str, presses: usize) -> Shown<'_> {
        Shown {
            first,
            presses,
            then: "",
            presses_then: 0,
        }
    }
}

struct Menu {
    owned: Owned,
}

impl Menu {
    fn new() -> Menu {
        let mut owned = Owned::new();
        owned.spawn_mux(|_| {});
        Menu { owned }
    }

    fn opened(&self, shown: &Shown<'_>) -> api::TerminalKey {
        let terminal = self.opened_alone(shown);
        let home = Path::new(&self.owned.home()).to_owned();
        let pid = || fs::read_to_string(home.join("pid")).ok();
        let pid = eventually("the screen says its pid", pid);
        let pid: u32 = pid.trim().parse().unwrap();
        let sessions = home.join(".claude/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let file = file_of_pid("menu", pid, "idle").to_string();
        fs::write(sessions.join(format!("{pid}.json")), file).unwrap();
        let folder = luned::folder::open(&self.owned.host.folder())
            .unwrap()
            .unwrap();
        eventually("luned lists the agent in the terminal", || {
            let folder = folder.clone();
            let listed = self.owned.block_on(async move {
                let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
                daemon.list_agents(()).await.unwrap().into_inner().agents
            });
            listed
                .iter()
                .any(|agent| agent.terminal == Some(terminal))
                .then_some(())
        });
        terminal
    }

    fn opened_alone(&self, shown: &Shown<'_>) -> api::TerminalKey {
        let script = Path::new(&self.owned.home()).join("screen.sh");
        fs::write(&script, SCREEN).unwrap();
        let (presses, presses_then) = (shown.presses.to_string(), shown.presses_then.to_string());
        let argv = [
            script.to_str().unwrap(),
            shown.first,
            &presses,
            shown.then,
            &presses_then,
        ];
        let argv = ["sh"].into_iter().chain(argv).map(str::to_owned).collect();
        let mux_api::TerminalKey { id, made_at } = self.owned.new_terminal_running(b"menu", argv);
        api::TerminalKey { id, made_at }
    }

    fn waiting(&self, terminal: &api::TerminalKey) -> Result<WaitingResponse, tonic::Status> {
        let folder = luned::folder::open(&self.owned.host.folder())
            .unwrap()
            .unwrap();
        let request = WaitingRequest {
            on: None,
            terminal: Some(*terminal),
        };
        self.owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon
                .waiting(request)
                .await
                .map(tonic::Response::into_inner)
        })
    }

    fn answer(
        &self,
        terminal: &api::TerminalKey,
        shown: u64,
        answer: answer_request::Answer,
    ) -> Result<(), tonic::Status> {
        let folder = luned::folder::open(&self.owned.host.folder())
            .unwrap()
            .unwrap();
        let (terminal, shown, answer) = (Some(*terminal), Some(shown), Some(answer));
        let request = AnswerRequest {
            on: None,
            terminal,
            shown,
            answer,
        };
        self.owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon.answer(request).await.map(drop)
        })
    }

    fn pressed(&self) -> String {
        let pressed = Path::new(&self.owned.home()).join("pressed");
        eventually("the menu heard its keys", || {
            fs::read_to_string(&pressed).ok()
        })
        .trim()
        .to_owned()
    }

    fn shown(&self, terminal: &api::TerminalKey, text: &str) -> WaitingResponse {
        eventually_showing("the terminal shows its screen", || {
            let waiting = self
                .waiting(terminal)
                .map_err(|status| status.message().to_owned())?;
            if waiting.screen.contains(text) {
                Ok(waiting)
            } else {
                Err(waiting.screen)
            }
        })
    }
}

fn choice(index: u32, label: &str) -> answer_request::Answer {
    answer_request::Answer::Choice(answer_request::Choice {
        index,
        label: label.to_owned(),
    })
}

fn text(text: &str) -> answer_request::Answer {
    answer_request::Answer::Text(text.to_owned())
}

fn menu_of(choices: &[&str], chosen: u32) -> Asks {
    let choices = choices.iter().map(|&choice| choice.to_owned()).collect();
    Asks::Menu(Choices { choices, chosen })
}

#[test]
fn a_waiting_terminal_shows_its_screen_and_choices() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 2));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    assert_eq!(waiting.asks, Some(menu_of(&["Yes", "No"], 0)));
    assert!(
        waiting.screen.contains("Do you want to proceed?"),
        "{}",
        waiting.screen
    );
    assert_eq!(waiting.said, None);
}

#[test]
fn a_terminal_with_no_menu_asks_for_words() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(PROMPT, 2));
    let waiting = menu.shown(&terminal, "What next?");
    assert_eq!(waiting.asks, Some(Asks::Words(())));
}

#[test]
fn a_terminal_that_runs_no_agent_is_shown_and_not_answered() {
    let menu = Menu::new();
    let terminal = menu.opened_alone(&Shown::still(MENU, 1));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    let unknown = format!("terminal {} runs no agent luned knows", terminal.id);
    assert_eq!(waiting.asks, Some(Asks::Unread(unknown.clone())));
    let refused = menu
        .answer(&terminal, waiting.shown, choice(0, "Yes"))
        .unwrap_err();
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::FailedPrecondition, unknown.as_str())
    );
}

#[test]
fn an_answer_to_a_screen_that_changed_since_it_was_read_presses_nothing() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 1));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    let escape = answer_request::Answer::Escape(());
    let refused = menu
        .answer(&terminal, waiting.shown.wrapping_add(1), escape)
        .unwrap_err();
    assert_eq!(refused.code(), Code::Aborted);
    assert_eq!(
        refused.message(),
        "the screen changed since it was read: read it again"
    );
    menu.answer(&terminal, waiting.shown, choice(0, "Yes"))
        .unwrap();
    assert_eq!(menu.pressed(), "0d");
}

#[test]
fn a_choice_moves_the_cursor_to_it_and_presses_enter_once_it_is_there() {
    let menu = Menu::new();
    let shown = Shown {
        first: MENU,
        presses: 3,
        then: MOVED,
        presses_then: 1,
    };
    let terminal = menu.opened(&shown);
    let waiting = menu.shown(&terminal, "Esc to cancel");
    menu.answer(&terminal, waiting.shown, choice(1, "No"))
        .unwrap();
    assert_eq!(menu.pressed(), "1b 5b 42 0d");
}

#[test]
fn a_choice_the_cursor_does_not_reach_is_not_entered() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 3));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    let refused = menu
        .answer(&terminal, waiting.shown, choice(1, "No"))
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    let unreached = r#"the cursor did not reach choice 2, "No", so Enter was not pressed"#;
    assert_eq!(refused.message(), unreached);
    assert_eq!(menu.pressed(), "1b 5b 42");
}

#[test]
fn a_choice_is_not_entered_on_another_menu_that_asks_anew() {
    let menu = Menu::new();
    let shown = Shown {
        first: ON_NO,
        presses: 3,
        then: ASKED_ANEW,
        presses_then: 1,
    };
    let terminal = menu.opened(&shown);
    let waiting = menu.shown(&terminal, "Esc to cancel");
    assert_eq!(waiting.asks, Some(menu_of(&["Yes", "No"], 1)));
    let refused = menu
        .answer(&terminal, waiting.shown, choice(0, "Yes"))
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    let unreached = r#"the cursor did not reach choice 1, "Yes", so Enter was not pressed"#;
    assert_eq!(refused.message(), unreached);
    let anew = menu.waiting(&terminal).unwrap();
    assert!(
        anew.screen.contains("Run rm -rf ./work?"),
        "{}",
        anew.screen
    );
}

#[test]
fn a_prompt_sent_above_the_input_is_no_menu_and_words_answer_it() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(SENT, 6));
    let waiting = menu.shown(&terminal, "Opus 5.5");
    assert_eq!(waiting.asks, Some(Asks::Words(())));
    menu.answer(&terminal, waiting.shown, text("hello"))
        .unwrap();
    assert_eq!(menu.pressed(), "68 65 6c 6c 6f 0d");
}

#[test]
fn a_choice_the_screen_does_not_show_is_refused() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 4));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    let refused = menu
        .answer(&terminal, waiting.shown, choice(1, "Maybe"))
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert_eq!(refused.message(), r#"choice 2 is now "No", not "Maybe""#);
}

#[test]
fn a_text_answer_is_typed_and_entered() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(PROMPT, 6));
    let waiting = menu.shown(&terminal, "What next?");
    menu.answer(&terminal, waiting.shown, text("hello"))
        .unwrap();
    assert_eq!(menu.pressed(), "68 65 6c 6c 6f 0d");
}

#[test]
fn a_text_answer_is_refused_while_the_screen_shows_choices() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 6));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    let refused = menu
        .answer(&terminal, waiting.shown, text("hello"))
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert_eq!(
        refused.message(),
        "the screen shows 2 choices: answer with one of them"
    );
}

#[test]
fn escape_is_pressed_alone() {
    let menu = Menu::new();
    let terminal = menu.opened(&Shown::still(MENU, 1));
    let waiting = menu.shown(&terminal, "Esc to cancel");
    menu.answer(&terminal, waiting.shown, answer_request::Answer::Escape(()))
        .unwrap();
    assert_eq!(menu.pressed(), "1b");
}
