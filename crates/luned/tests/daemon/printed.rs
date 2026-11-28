use std::fs;

use lune_waits::eventually;
use luned::api::keep_printed_response::Printed;
use luned::api::{self, KeepPrintedRequest, KeepPrintedResponse};
use luned::mux::own::api as mux_api;
use tonic::Code;

use crate::host::PATIENT;
use crate::own::Owned;

const PRINTS: &str = r"printf '$ make\r\nbuilt it\r\n$ '; sleep 600";

struct Shell {
    owned: Owned,
}

impl Shell {
    fn new() -> Shell {
        let mut owned = Owned::new();
        owned.spawn_mux(|_| {});
        Shell { owned }
    }

    fn printing(&self) -> api::TerminalKey {
        let argv = ["sh", "-c", PRINTS].map(str::to_owned).to_vec();
        let terminal = self.owned.new_terminal_running(b"shell", argv);
        eventually("the shell prints its prompt again", || {
            let dumped = self.owned.dumped(terminal);
            dumped.contains("built it\n$").then_some(())
        });
        let mux_api::TerminalKey { id, made_at } = terminal;
        api::TerminalKey { id, made_at }
    }

    fn kept(
        &self,
        terminal: Option<api::TerminalKey>,
        typed: &str,
    ) -> Result<KeepPrintedResponse, tonic::Status> {
        let folder = luned::folder::open(&self.owned.host.folder())
            .unwrap()
            .unwrap();
        let request = KeepPrintedRequest {
            on: None,
            terminal,
            typed: typed.to_owned(),
        };
        self.owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon
                .keep_printed(request)
                .await
                .map(tonic::Response::into_inner)
        })
    }
}

#[test]
fn luned_keeps_what_a_command_printed_in_a_terminal_of_this_machine() {
    let shell = Shell::new();
    let terminal = shell.printing();
    let kept = shell.kept(Some(terminal), "make").unwrap();
    let Some(Printed::After(file)) = kept.printed else {
        panic!("what make printed is kept: {kept:?}");
    };
    assert_eq!((file.lines, file.left_out), (1, 0));
    assert!(
        file.path
            .starts_with(shell.owned.host.folder().to_str().unwrap()),
        "{}",
        file.path
    );
    assert_eq!(fs::read_to_string(&file.path).unwrap(), "built it\n");

    let untyped = shell.kept(Some(terminal), "").unwrap_err();
    assert_eq!(
        (untyped.code(), untyped.message()),
        (Code::InvalidArgument, "typed: nothing")
    );
    let unnamed = shell.kept(None, "make").unwrap_err();
    let said = (unnamed.code(), unnamed.message());
    assert_eq!(said, (Code::InvalidArgument, "it names no terminal"));
}
