use std::sync::Arc;

use lune_domain::projects::Opens;
use tonic::{Request, Response, Status, Streaming};

use super::Host;
use crate::api::change_projects_request::Change;
use crate::api::daemon_server::Daemon;
use crate::api::{
    self, AnswerRequest, AskRequest, ChangeProjectsRequest, ChangeProjectsResponse,
    FileIssueRequest, FramesRequest, InfoResponse, IssueRepositoriesRequest,
    IssueRepositoriesResponse, KeepChecksRequest, KeepImageRequest, KeepPrintedRequest,
    KeepPrintedResponse, ListAgentsResponse,
    ListContentsRequest, ListContentsResponse, ListRepositoriesRequest, ListRepositoriesResponse,
    ListSkillsRequest, ListSkillsResponse, ListWorkRequest, MakeWorktreeRequest, PasteRequest,
    PruneRequest, PrunesRequest, ReadDiffRequest, RebaseRequest, SayRequest, ShowRequest,
    TranscriptEndsRequest,
    TranscriptEndsResponse, WaitingRequest, WaitingResponse,
};
use crate::mux::own::api::FromViewer;

pub(super) struct OverTailnet(pub(super) Arc<Host>);

fn unserved(call: &str) -> Status {
    Status::permission_denied(format!("{call} is not served over the tailnet, only on this machine"))
}

fn served_change(change: Option<&Change>) -> Result<(), Status> {
    let unserved_change = |change: &str| Err(unserved(&format!("ChangeProjects {change}")));
    match change {
        Some(
            Change::CloseView(_)
            | Change::CloseWorktree(_)
            | Change::RenameProject(_)
            | Change::RenameWorktree(_)
            | Change::RenameTerminal(_)
            | Change::ReorderProjects(_)
            | Change::ReorderWorktrees(_),
        ) => Ok(()),
        None => Err(unserved("ChangeProjects with no change")),
        Some(Change::AddProject(_)) => unserved_change("AddProject"),
        Some(Change::AddWorktree(_)) => unserved_change("AddWorktree"),
        Some(Change::Start(_)) => unserved_change("Start"),
        Some(Change::Join(_)) => unserved_change("Join"),
        Some(Change::Pull(_)) => unserved_change("Pull"),
        Some(Change::ForgetProject(_)) => unserved_change("ForgetProject"),
        Some(Change::CloseProject(_)) => unserved_change("CloseProject"),
        Some(Change::Bring(_)) => unserved_change("Bring"),
    }
}

fn served_opens(opens: Option<&api::Opens>) -> Result<(), Status> {
    let Some(opens) = opens else {
        return Err(unserved("MakeWorktree that opens nothing, and so a shell"));
    };
    let unopenable = |why: String| Status::invalid_argument(format!("opens: {why}"));
    match Opens::of_api(Some(opens.clone())).map_err(unopenable)? {
        Opens::Agent(_) => Ok(()),
        Opens::Shell => Err(unserved("MakeWorktree that opens a shell")),
        Opens::Command(_) => Err(unserved("MakeWorktree that opens a command")),
    }
}

fn served_said(said: &FromViewer) -> Result<(), Status> {
    use crate::mux::own::api::from_viewer::Event;
    use crate::mux::own::api::typed::Event as Typed;
    use crate::mux::own::api::Typed as Said;
    let unserved_typed = |typed: &str| Err(unserved(&format!("Frames typing {typed}")));
    let typed = match &said.event {
        Some(Event::Typed(Said { event, .. })) => event.as_ref(),
        Some(Event::Hello(_) | Event::Showing(_)) | None => return Ok(()),
    };
    match typed {
        Some(Typed::Key(_) | Typed::Paste(_) | Typed::Scroll(_)) | None => Ok(()),
        Some(Typed::Mouse(_)) => unserved_typed("a mouse"),
        Some(Typed::Focus(_)) => unserved_typed("a focus"),
    }
}

#[tonic::async_trait]
impl Daemon for OverTailnet {
    type AskStream = <Host as Daemon>::AskStream;
    type FileIssueStream = <Host as Daemon>::FileIssueStream;
    type FramesStream = <Host as Daemon>::FramesStream;
    type KeepChecksStream = <Host as Daemon>::KeepChecksStream;
    type KeepImageStream = <Host as Daemon>::KeepImageStream;
    type ListWorkStream = <Host as Daemon>::ListWorkStream;
    type MakeWorktreeStream = <Host as Daemon>::MakeWorktreeStream;
    type PasteStream = <Host as Daemon>::PasteStream;
    type PruneStream = <Host as Daemon>::PruneStream;
    type PrunesStream = <Host as Daemon>::PrunesStream;
    type ReadDiffStream = <Host as Daemon>::ReadDiffStream;
    type RebaseStream = <Host as Daemon>::RebaseStream;
    type ShowStream = <Host as Daemon>::ShowStream;

    async fn answer(&self, request: Request<AnswerRequest>) -> Result<Response<()>, Status> {
        Daemon::answer(&*self.0, request).await
    }

    async fn ask(&self, request: Request<AskRequest>) -> Result<Response<Self::AskStream>, Status> {
        Daemon::ask(&*self.0, request).await
    }

    async fn change_projects(
        &self,
        request: Request<ChangeProjectsRequest>,
    ) -> Result<Response<ChangeProjectsResponse>, Status> {
        served_change(request.get_ref().change.as_ref())?;
        Daemon::change_projects(&*self.0, request).await
    }

    async fn file_issue(
        &self,
        request: Request<FileIssueRequest>,
    ) -> Result<Response<Self::FileIssueStream>, Status> {
        Daemon::file_issue(&*self.0, request).await
    }

    async fn issue_repositories(
        &self,
        request: Request<IssueRepositoriesRequest>,
    ) -> Result<Response<IssueRepositoriesResponse>, Status> {
        Daemon::issue_repositories(&*self.0, request).await
    }

    async fn list_work(
        &self,
        request: Request<ListWorkRequest>,
    ) -> Result<Response<Self::ListWorkStream>, Status> {
        Daemon::list_work(&*self.0, request).await
    }

    async fn read_diff(
        &self,
        _: Request<ReadDiffRequest>,
    ) -> Result<Response<Self::ReadDiffStream>, Status> {
        Err(unserved("ReadDiff"))
    }

    async fn make_worktree(
        &self,
        request: Request<MakeWorktreeRequest>,
    ) -> Result<Response<Self::MakeWorktreeStream>, Status> {
        served_opens(request.get_ref().opens.as_ref())?;
        Daemon::make_worktree(&*self.0, request).await
    }

    async fn prune(
        &self,
        request: Request<PruneRequest>,
    ) -> Result<Response<Self::PruneStream>, Status> {
        Daemon::prune(&*self.0, request).await
    }

    async fn prunes(
        &self,
        request: Request<PrunesRequest>,
    ) -> Result<Response<Self::PrunesStream>, Status> {
        Daemon::prunes(&*self.0, request).await
    }

    async fn rebase(
        &self,
        request: Request<RebaseRequest>,
    ) -> Result<Response<Self::RebaseStream>, Status> {
        Daemon::rebase(&*self.0, request).await
    }

    async fn waiting(
        &self,
        request: Request<WaitingRequest>,
    ) -> Result<Response<WaitingResponse>, Status> {
        Daemon::waiting(&*self.0, request).await
    }

    async fn watch(
        &self,
        request: Request<api::WatchRequest>,
    ) -> Result<Response<api::Host>, Status> {
        Daemon::watch(&*self.0, request).await
    }

    async fn info(&self, _: Request<()>) -> Result<Response<InfoResponse>, Status> {
        Err(unserved("Info"))
    }

    async fn list_agents(&self, _: Request<()>) -> Result<Response<ListAgentsResponse>, Status> {
        Err(unserved("ListAgents"))
    }

    async fn list_skills(
        &self,
        _: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        Err(unserved("ListSkills"))
    }

    async fn keep_checks(
        &self,
        _: Request<KeepChecksRequest>,
    ) -> Result<Response<Self::KeepChecksStream>, Status> {
        Err(unserved("KeepChecks"))
    }

    async fn keep_printed(
        &self,
        _: Request<KeepPrintedRequest>,
    ) -> Result<Response<KeepPrintedResponse>, Status> {
        Err(unserved("KeepPrinted"))
    }

    async fn list_repositories(
        &self,
        _: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        Err(unserved("ListRepositories"))
    }

    async fn list_contents(
        &self,
        _: Request<ListContentsRequest>,
    ) -> Result<Response<ListContentsResponse>, Status> {
        Err(unserved("ListContents"))
    }

    async fn transcript_ends(
        &self,
        _: Request<TranscriptEndsRequest>,
    ) -> Result<Response<TranscriptEndsResponse>, Status> {
        Err(unserved("TranscriptEnds"))
    }

    async fn paste(&self, _: Request<PasteRequest>) -> Result<Response<Self::PasteStream>, Status> {
        Err(unserved("Paste"))
    }

    async fn keep_image(
        &self,
        _: Request<Streaming<KeepImageRequest>>,
    ) -> Result<Response<Self::KeepImageStream>, Status> {
        Err(unserved("KeepImage"))
    }

    async fn say(&self, _: Request<SayRequest>) -> Result<Response<()>, Status> {
        Err(unserved("Say"))
    }

    async fn show(
        &self,
        _: Request<Streaming<ShowRequest>>,
    ) -> Result<Response<Self::ShowStream>, Status> {
        Err(unserved("Show"))
    }

    async fn frames(
        &self,
        request: Request<Streaming<FramesRequest>>,
    ) -> Result<Response<Self::FramesStream>, Status> {
        self.0.frames_served(request, served_said).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, TryRecvError};
    use std::sync::Arc;
    use std::time::SystemTime;

    use hyper_util::rt::TokioIo;
    use tokio::net::{UnixListener, UnixStream};
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::transport::{Endpoint, Server, Uri};
    use tonic::Code;
    use tower::service_fn;

    use super::{served_change, served_opens, served_said, Host, OverTailnet};
    use crate::api::change_projects_request::{self as change, Change};
    use crate::api::daemon_client::DaemonClient;
    use crate::api::daemon_server::DaemonServer;
    use crate::api::opens::Runs;
    use crate::api::{self, KeepImageRequest, ShowRequest, FramesRequest};

    #[test]
    fn a_tailnet_device_changes_projects_only_by_closing_renaming_and_reordering() {
        let served = [
            Change::CloseView(change::CloseView::default()),
            Change::CloseWorktree(change::CloseWorktree::default()),
            Change::RenameProject(change::RenameProject::default()),
            Change::RenameWorktree(change::RenameWorktree::default()),
            Change::RenameTerminal(change::RenameTerminal::default()),
            Change::ReorderProjects(change::ReorderProjects::default()),
            Change::ReorderWorktrees(change::ReorderWorktrees::default()),
        ];
        for change in &served {
            assert!(served_change(Some(change)).is_ok(), "{change:?}");
        }
        let unserved = [
            ("AddProject", Some(Change::AddProject(change::AddProject::default()))),
            ("AddWorktree", Some(Change::AddWorktree(change::AddWorktree::default()))),
            ("Start", Some(Change::Start(change::Start::default()))),
            ("Join", Some(Change::Join(change::Join::default()))),
            ("Pull", Some(Change::Pull(change::Pull::default()))),
            ("ForgetProject", Some(Change::ForgetProject(change::ForgetProject::default()))),
            ("CloseProject", Some(Change::CloseProject(change::CloseProject::default()))),
            ("Bring", Some(Change::Bring(change::Bring::default()))),
            ("with no change", None),
        ];
        for (named, change) in unserved {
            let refused = served_change(change.as_ref()).unwrap_err();
            assert_eq!(refused.code(), Code::PermissionDenied, "{named}");
            let message = format!("ChangeProjects {named} is not served over the tailnet, only on this machine");
            assert_eq!(refused.message(), message);
        }
    }

    #[test]
    fn a_tailnet_device_makes_a_worktree_only_to_open_an_agent() {
        let opens = |runs| api::Opens { runs };
        let prompted = api::Prompted { harness: api::Harness::ClaudeCode.into(), words: "Tackle #741".to_owned() };
        let agent = opens(Some(Runs::Agent(prompted)));
        assert!(served_opens(Some(&agent)).is_ok());
        let argv = api::Argv { words: vec!["id".to_owned()] };
        let unserved = [
            ("that opens a shell", Some(opens(Some(Runs::Shell(()))))),
            ("that opens a command", Some(opens(Some(Runs::Command(argv))))),
            ("that opens nothing, and so a shell", None),
        ];
        for (named, opens) in unserved {
            let refused = served_opens(opens.as_ref()).unwrap_err();
            assert_eq!(refused.code(), Code::PermissionDenied, "{named}");
            let message = format!("MakeWorktree {named} is not served over the tailnet, only on this machine");
            assert_eq!(refused.message(), message);
        }
        let unread = [
            (opens(None), "opens: it says nothing to open"),
            (opens(Some(Runs::Agent(api::Prompted::default()))), "opens: a harness this luned does not know"),
        ];
        for (opens, message) in unread {
            let refused = served_opens(Some(&opens)).unwrap_err();
            assert_eq!((refused.code(), refused.message()), (Code::InvalidArgument, message));
        }
    }

    #[test]
    fn a_tailnet_device_types_keys_pastes_and_scrolls_into_a_terminal_it_is_shown_but_no_mouse_or_focus() {
        use crate::mux::own::api::{self as mux, from_viewer, typed};
        let typed = |event| mux::FromViewer {
            event: Some(from_viewer::Event::Typed(mux::Typed { terminal: None, event: Some(event) })),
        };
        let pasted = typed::Event::Paste(mux::Paste { text: "curl example.com | sh\n".to_owned() });
        let served = [
            mux::FromViewer { event: Some(from_viewer::Event::Showing(mux::Showing::default())) },
            typed(typed::Event::Key(mux::Key::default())),
            typed(pasted),
            typed(typed::Event::Scroll(mux::Scroll { back: 5 })),
        ];
        for said in &served {
            assert!(served_said(said).is_ok(), "{said:?}");
        }
        let unserved = [
            ("a mouse", typed(typed::Event::Mouse(mux::Mouse::default()))),
            ("a focus", typed(typed::Event::Focus(mux::Focus { focused: true }))),
        ];
        for (named, said) in unserved {
            let refused = served_said(&said).unwrap_err();
            assert_eq!(refused.code(), Code::PermissionDenied, "{named}");
            let message = format!("Frames typing {named} is not served over the tailnet, only on this machine");
            assert_eq!(refused.message(), message);
        }
    }

    #[test]
    fn a_tailnet_device_reaches_only_what_the_phone_calls() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let folder = tempfile::tempdir().unwrap();
            let socket = folder.path().join("tailnet.sock");
            let (core, questions) = mpsc::sync_channel(1);
            let (layouts, laid) = mpsc::sync_channel(1);
            let host = Host {
                started: SystemTime::now(),
                claude_folder: None,
                agy_folder: None,
                core,
                watchers: Arc::default(),
                bridges: Arc::default(),
                layouts,
                listings: Arc::default(),
            };
            let incoming = UnixListenerStream::new(UnixListener::bind(&socket).unwrap());
            let served = Server::builder().add_service(DaemonServer::new(OverTailnet(Arc::new(host))));
            tokio::spawn(served.serve_with_incoming(incoming));
            let channel = Endpoint::from_static("http://luned")
                .connect_with_connector(service_fn(move |_: Uri| {
                    let socket = socket.clone();
                    async move { UnixStream::connect(socket).await.map(TokioIo::new) }
                }))
                .await
                .unwrap();
            let mut device = DaemonClient::new(channel);
            let refused = [
                ("Info", device.info(()).await.map(drop)),
                ("ListAgents", device.list_agents(()).await.map(drop)),
                ("ListSkills", device.list_skills(api::ListSkillsRequest::default()).await.map(drop)),
                ("KeepChecks", device.keep_checks(api::KeepChecksRequest::default()).await.map(drop)),
                ("KeepPrinted", device.keep_printed(api::KeepPrintedRequest::default()).await.map(drop)),
                ("ListRepositories", device.list_repositories(api::ListRepositoriesRequest::default()).await.map(drop)),
                ("ListContents", device.list_contents(api::ListContentsRequest::default()).await.map(drop)),
                ("TranscriptEnds", device.transcript_ends(api::TranscriptEndsRequest::default()).await.map(drop)),
                ("Paste", device.paste(api::PasteRequest::default()).await.map(drop)),
                ("KeepImage", device.keep_image(tokio_stream::iter(Vec::<KeepImageRequest>::new())).await.map(drop)),
                ("Say", device.say(api::SayRequest::default()).await.map(drop)),
                ("ReadDiff", device.read_diff(api::ReadDiffRequest::default()).await.map(drop)),
                ("Show", device.show(tokio_stream::iter(Vec::<ShowRequest>::new())).await.map(drop)),
            ];
            let changed = |change| api::ChangeProjectsRequest { on: None, change: Some(change) };
            let opening = |runs: Option<Runs>| api::MakeWorktreeRequest { opens: runs.map(|runs| api::Opens { runs: Some(runs) }), ..Default::default() };
            let argv = || api::Argv { words: vec!["sh".to_owned(), "-c".to_owned(), "id".to_owned()] };
            let worktree = change::AddWorktree { opens: Some(api::Opens { runs: Some(Runs::Command(argv())) }), ..Default::default() };
            let pull = change::Pull { agent: Some(argv()), ..Default::default() };
            let narrowed = [
                ("ChangeProjects AddProject", device.change_projects(changed(Change::AddProject(change::AddProject::default()))).await.map(drop)),
                ("ChangeProjects AddWorktree", device.change_projects(changed(Change::AddWorktree(worktree))).await.map(drop)),
                ("ChangeProjects Pull", device.change_projects(changed(Change::Pull(pull))).await.map(drop)),
                ("ChangeProjects Join", device.change_projects(changed(Change::Join(change::Join::default()))).await.map(drop)),
                ("MakeWorktree that opens a shell", device.make_worktree(opening(Some(Runs::Shell(())))).await.map(drop)),
                ("MakeWorktree that opens a command", device.make_worktree(opening(Some(Runs::Command(argv())))).await.map(drop)),
                ("MakeWorktree that opens nothing", device.make_worktree(opening(None)).await.map(drop)),
            ];
            for (call, refused) in refused.into_iter().chain(narrowed) {
                let refused = refused.unwrap_err();
                assert_eq!(refused.code(), Code::PermissionDenied, "{call}: {refused:?}");
                assert!(refused.message().starts_with(call), "{call}: {}", refused.message());
                assert!(refused.message().contains("not served over the tailnet"), "{call}: {}", refused.message());
            }
            assert!(matches!(questions.try_recv(), Err(TryRecvError::Empty)));
            assert!(matches!(laid.try_recv(), Err(TryRecvError::Empty)));
            drop((questions, laid));
            let unhello = device.frames(tokio_stream::iter(Vec::<FramesRequest>::new())).await.err();
            let unhello = unhello.expect("frames that say no hello are refused");
            assert_eq!((unhello.code(), unhello.message()), (Code::InvalidArgument, "the first word is no hello"));
            let stopped = (Code::Unavailable, "the daemon's core has stopped");
            let watched = device.watch(api::WatchRequest::default()).await.unwrap_err();
            assert_eq!((watched.code(), watched.message()), stopped);
            let renamed = change::RenameProject { project: 1, to: "lune".to_owned() };
            let renamed = device.change_projects(changed(Change::RenameProject(renamed))).await.unwrap_err();
            assert_eq!(renamed.code(), Code::Unavailable, "{renamed:?}");
        });
    }
}
