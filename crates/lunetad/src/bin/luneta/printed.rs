use std::sync::mpsc::Sender;
use std::thread;

use luneta_protocol::said::Said;
use luneta_protocol::shell::Ending;
pub use lunetad::printed::{Kept, Printed};
use lunetad::SessionName;

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::Ended;
use crate::projected::Route;

pub fn prompt(printed: &Printed, asks: &str, ended: &Ended) -> String {
    let cannot = "with an exit status my shell cannot say";
    let (how, after, line) = match ended.ending {
        Some(Ending::Exited(status)) => {
            (format!("ended with exit status {status}"), "", "the line I ran it on")
        },
        None => (format!("ended, {cannot}"), "", "the line I ran it on"),
        Some(Ending::Resumed) => (
            format!("ended once I brought it back with `fg`, {cannot}"),
            " after `fg`",
            "the `fg` I brought it back with",
        ),
    };
    let screen = |Kept { file, lines, left_out }: &Kept| {
        let holds = if *left_out == 0 {
            "all my screen has".to_owned()
        } else {
            format!("the last {lines} of the {} lines my screen has", lines + left_out)
        };
        format!("{file} holds {holds}")
    };
    let printed = match printed {
        Printed::After(Kept { file, left_out: 0, .. }) => {
            format!("What it printed{after} is in {file}.")
        },
        Printed::After(Kept { file, lines, left_out }) => {
            let of = lines + left_out;
            format!("The last {lines} of the {of} lines it printed{after} are in {file}.")
        },
        Printed::Nothing => format!("It printed nothing{after}."),
        Printed::Unsure(kept) => format!(
            "Nothing follows the last line on my screen that looks like {line}, which may \
             be one it printed, so {}.",
            screen(kept)
        ),
        Printed::Top(None) => format!("My screen no longer shows {line}, nor anything else."),
        Printed::Top(Some(kept)) => format!(
            "My screen no longer shows {line}, so {}, which may begin before what it printed.",
            screen(kept)
        ),
    };
    format!("{asks} In my shell, `{}` {how}. {printed}", ended.command.as_str())
}

pub fn read(
    menu: u64,
    on: &Machine,
    route: Result<Route, String>,
    session: &SessionName,
    pane: u32,
    ended: Ended,
    events: &Sender<Event>,
) {
    let (on, session) = (on.clone(), session.clone());
    let due = Due::new(events, move |printed| Event::Printed { menu, printed });
    thread::spawn(move || {
        let typed = typed(&ended);
        let printed = match (route, &on) {
            (Err(why), _) => Err(why),
            (Ok(Route::Terminal(terminal)), _) => lunetad::printed::kept_in(terminal, &typed),
            (Ok(Route::Zellij), Machine::Local) => {
                lunetad::printed::kept_after(&session, pane, &typed)
            },
            (Ok(Route::Zellij), Machine::Remote(destination)) => {
                Away::reach(destination, away::ANSWER)
                    .and_then(|mut away| away.printed(&session, pane, &typed))
            },
        };
        due.pay(printed);
    });
}

fn typed(ended: &Ended) -> Said {
    match ended.ending {
        Some(Ending::Resumed) => Said::new("fg".to_owned()).expect("`fg` is no empty line"),
        Some(Ending::Exited(_)) | None => ended.command.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    fn ended(command: &str, ending: Option<Ending>) -> Ended {
        Ended { command: Said::new(command.to_owned()).unwrap(), ending, at: UNIX_EPOCH }
    }

    #[test]
    fn a_job_brought_back_was_typed_as_fg() {
        let typed = |ended: Ended| typed(&ended).as_str().to_owned();
        assert_eq!(typed(ended("cargo test", Some(Ending::Exited(101)))), "cargo test");
        assert_eq!(typed(ended("vim", None)), "vim");
        assert_eq!(typed(ended("cargo build", Some(Ending::Resumed))), "fg");
        assert_eq!(typed(ended("fg %1", Some(Ending::Resumed))), "fg");
    }

    #[test]
    fn the_agent_is_told_the_command_how_it_ended_and_where_what_it_printed_is() {
        let file = "/run/luneta/printed/1.txt".to_owned();
        let kept = |lines, left_out| Kept { file: file.clone(), lines, left_out };
        let failed = ended("cargo test", Some(Ending::Exited(101)));
        let told = |printed: Printed| prompt(&printed, "Fix this.", &failed);
        let ran = "Fix this. In my shell, `cargo test` ended with exit status 101.";
        let gone = "My screen no longer shows the line I ran it on";
        assert_eq!(
            told(Printed::After(kept(2, 0))),
            format!("{ran} What it printed is in /run/luneta/printed/1.txt.")
        );
        assert_eq!(
            told(Printed::After(kept(2000, 7))),
            format!(
                "{ran} The last 2000 of the 2007 lines it printed are in \
                 /run/luneta/printed/1.txt."
            )
        );
        assert_eq!(told(Printed::Nothing), format!("{ran} It printed nothing."));
        assert_eq!(
            told(Printed::Unsure(kept(4, 0))),
            format!(
                "{ran} Nothing follows the last line on my screen that looks like the line I ran \
                 it on, which may be one it printed, so /run/luneta/printed/1.txt holds all my \
                 screen has."
            )
        );
        assert_eq!(
            told(Printed::Top(Some(kept(2, 0)))),
            format!(
                "{ran} {gone}, so /run/luneta/printed/1.txt holds all my screen has, which may \
                 begin before what it printed."
            )
        );
        assert_eq!(
            told(Printed::Top(Some(kept(2000, 1)))),
            format!(
                "{ran} {gone}, so /run/luneta/printed/1.txt holds the last 2000 of the 2001 lines \
                 my screen has, which may begin before what it printed."
            )
        );
        assert_eq!(told(Printed::Top(None)), format!("{ran} {gone}, nor anything else."));
        assert_eq!(
            prompt(&Printed::Nothing, "Explain this.", &ended("vim", None)),
            "Explain this. In my shell, `vim` ended, with an exit status my shell cannot say. It \
             printed nothing."
        );

        let resumed = ended("cargo build", Some(Ending::Resumed));
        let told = |printed: Printed| prompt(&printed, "Fix this.", &resumed);
        let ran = "Fix this. In my shell, `cargo build` ended once I brought it back with `fg`, \
                   with an exit status my shell cannot say.";
        assert_eq!(
            told(Printed::After(kept(2, 0))),
            format!("{ran} What it printed after `fg` is in /run/luneta/printed/1.txt.")
        );
        assert_eq!(
            told(Printed::After(kept(2000, 7))),
            format!(
                "{ran} The last 2000 of the 2007 lines it printed after `fg` are in \
                 /run/luneta/printed/1.txt."
            )
        );
        assert_eq!(told(Printed::Nothing), format!("{ran} It printed nothing after `fg`."));
        assert_eq!(
            told(Printed::Top(None)),
            format!(
                "{ran} My screen no longer shows the `fg` I brought it back with, nor anything \
                 else."
            )
        );
    }
}
