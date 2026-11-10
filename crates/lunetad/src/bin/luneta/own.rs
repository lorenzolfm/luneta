//! Which client of its session is luneta's own, and so the pane you are in.
//!
//! zellij lists the clients of a session and the pane each has the focus on,
//! but says neither which is whose, nor, asked from outside, which pane one
//! client has the focus on. A client keeps the id zellij gave it as it came
//! to its session for as long as it stays there, though, and no other client
//! gets that id meanwhile. So luneta follows its own by its id, once it is
//! sure of it. It is when zellij, asked just after luneta's client came to
//! the session, lists one client; or one that was not listed before then; or,
//! just after luneta sent its client to a pane, one client alone on that pane.
//! A second client, as a plain `zellij attach` is, never moves the mark.
//!
//! The daemon lists the clients of every session every sweep, which can be
//! from before luneta's client came. So as it comes, luneta asks zellij
//! itself, and asks again after it sends the client to a pane: what zellij
//! answered stands until the daemon lists the clients other than it did when
//! luneta asked, or lists them still so once it has had the time to list
//! them again. Where luneta sends its client to a session itself, as it
//! starts or goes to a pane of another, it asks zellij for the session's
//! clients just before, and then until zellij lists one more, which is its
//! own. A move made in zellij, as by its keys, shows once the daemon lists it.
//!
//! A move to another session that luneta did not make, as by zellij's session
//! manager or the ask box's agent, gives its client a new id there, and zellij
//! may not title luneta's terminal as it comes. So once zellij no longer lists
//! the client luneta is sure of in the session it was in, luneta takes for its
//! own the one client that came to any session of the machine since the
//! daemon last listed its own where it was, or else says it cannot tell.

use std::time::{Duration, Instant};

use lunetad::api::Client;

/// How long what luneta saw of its client stands against a list of the
/// daemon's that is still the one it had then. The daemon asks zellij for
/// the clients of every session two seconds after it last asked, so that a
/// list it has after this is from after luneta looked: as when zellij's keys
/// take the client back where it was, or the pane it went to closes.
const TRUSTED: Duration = Duration::from_secs(3);

/// How long luneta looks for its client among the clients the daemon lists,
/// once it left the session it was in for one luneta cannot tell yet: the
/// daemon lists a session's clients again as zellij writes what it keeps of
/// it, which it does within a second of a client coming, and every sweep
/// besides. A client that comes after this is anyone's.
const HUNT: Duration = Duration::from_secs(10);

/// The clients of a session of the machine luneta's client is on, as the
/// daemon listed them: none where it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roster {
    pub session: String,
    pub clients: Option<Vec<Client>>,
}

/// What a list of the sessions of the machine says of the clients of one.
enum Was<'a> {
    /// It lists no session of that name.
    Absent,
    /// It lists the session, but not its clients, or more than one session
    /// of that name.
    Unlisted,
    Listed(&'a [Client]),
}

/// What `rosters` lists of the clients of `session`.
fn was<'a>(rosters: &'a [Roster], session: &str) -> Was<'a> {
    let mut named = rosters.iter().filter(|roster| roster.session == session);
    match (named.next(), named.next()) {
        (None, _) => Was::Absent,
        (Some(Roster { clients: Some(clients), .. }), None) => Was::Listed(clients),
        (Some(_), _) => Was::Unlisted,
    }
}

/// The clients `rosters` lists of `session`: none where they list none, or
/// more than one session of that name.
fn roster<'a>(rosters: &'a [Roster], session: &str) -> Option<&'a [Client]> {
    match was(rosters, session) {
        Was::Listed(clients) => Some(clients),
        Was::Absent | Was::Unlisted => None,
    }
}

/// The clients `rosters` lists that `seen` did not, of every session: all
/// those of one made since. None where `seen` did not list the clients of a
/// session that `rosters` lists any of, which may be anyone's.
fn newcomers<'r>(seen: &[Roster], rosters: &'r [Roster]) -> Option<Vec<(&'r str, Client)>> {
    let mut came = Vec::new();
    for listed in rosters {
        let clients = listed.clients.as_deref().unwrap_or_default();
        let before = match was(seen, &listed.session) {
            Was::Absent => &[][..],
            Was::Listed(before) => before,
            Was::Unlisted if clients.is_empty() => continue,
            Was::Unlisted => return None,
        };
        let new = clients.iter().filter(|client| before.iter().all(|was| was.id != client.id));
        came.extend(new.map(|client| (listed.session.as_str(), *client)));
    }
    Some(came)
}

/// What luneta asks zellij: the clients of `session`, as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub session: String,
    /// Counts the looks luneta asks for, by which one that comes back late is
    /// told from a newer one.
    pub number: u64,
    /// The pane luneta sent its client to just before, if it did.
    pub went: Option<u32>,
}

/// What zellij answered a look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Looked {
    pub number: u64,
    /// The pane luneta's client went to just before zellij was asked: the
    /// look's, unless zellij did not take it there.
    pub went: Option<u32>,
    pub clients: Vec<Client>,
    /// What luneta knew of the session's clients just before it sent its
    /// client there, from elsewhere or as it started, where the look was for
    /// the client's arrival: all of `clients` but luneta's own, as far as
    /// zellij said.
    pub before: Option<Before>,
}

/// What luneta knows of a session's clients before its own comes there, of
/// which none is luneta's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Before {
    /// zellij listed them: the session runs.
    Listed(Vec<Client>),
    /// No session ran by that name, as far as zellij's sockets said: zellij
    /// makes it, and luneta's client is its first. Its server cannot bear a call of
    /// zellij's CLI before it takes that client: every call asks the server
    /// of every session whether it runs, and zellij-server 0.45.1 panics
    /// where one hangs up before then (`lib.rs:1462`, fixed after 0.45.1 in
    /// zellij's 81f56e1), and the session ends with it.
    Made,
    /// zellij did not list them, as of a session whose server is gone or
    /// hangs: a client it lists after may be anyone's.
    Unknown,
}

impl Before {
    /// The clients that were there, as far as zellij said: none of a session
    /// it makes, nor of one it did not list.
    pub fn clients(&self) -> &[Client] {
        match self {
            Before::Listed(clients) => clients,
            Before::Made | Before::Unknown => &[],
        }
    }

    /// Whether the session's server may have yet to take its first client
    /// as luneta's comes: one zellij did not list the clients of.
    pub fn fresh(&self) -> bool {
        !matches!(self, Before::Listed(_))
    }
}

/// Whose word on the pane stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Word {
    /// The daemon's, as it lists the session's clients.
    #[default]
    Daemon,
    /// luneta's own, from a look or a go, until the daemon lists the clients
    /// other than as `listed`, as it did when luneta knew better, or still so
    /// from `until` on, by when it has listed them again.
    Own { listed: Option<Vec<Client>>, until: Instant },
}

/// What was listed of a session's clients before luneta's came to it: none
/// of them is luneta's.
#[derive(Debug)]
struct Others {
    clients: Vec<Client>,
    /// Whether zellij listed them, just before luneta sent its client there.
    /// The daemon's list can be older, and miss a client that came just
    /// before luneta's, which luneta would take for its own.
    by_zellij: bool,
}

/// Why luneta does not know the pane its client has the focus on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unplaced {
    /// luneta has yet to follow its client to a session, or zellij has yet
    /// to answer the look for it there.
    Waiting,
    /// luneta cannot tell its client from the others of the session: until the
    /// daemon lists them again, or luneta sends it to a pane, which it is then
    /// on.
    Unsure,
    /// It has the focus on a plugin's pane, which zellij lists no terminal
    /// for.
    Plugin,
}

/// luneta's client, as it was in the session luneta sent it from to another:
/// where it still is, if zellij does not take it.
#[derive(Debug)]
struct Left {
    session: String,
    id: u32,
    sure: bool,
    pane: Option<u32>,
    seen: Option<Vec<Roster>>,
}

/// luneta's client, which left the session it was in with no go of luneta's,
/// for one luneta looks for it in.
#[derive(Debug)]
struct Astray {
    /// The session it left.
    left: String,
    /// What the daemon listed of every session as it last listed the client
    /// in the one it left, until luneta gives up looking: none of the clients
    /// it listed is luneta's.
    seen: Option<Vec<Roster>>,
    until: Instant,
}

/// luneta's own client of zellij: the session it is in, its id there once
/// luneta is sure of it, and the pane it has the focus on.
#[derive(Debug, Default)]
pub struct OwnClient {
    /// The session it is in, as the sidebar knows it.
    session: Option<String>,
    id: Option<u32>,
    /// Whether zellij's word gave luneta `id`: the one client it listed, or
    /// the one it did not list just before luneta sent its client there. A go
    /// that finds that client on a pane other than the one luneta sent it to
    /// does not take the id back: zellij moved another client, or none. One
    /// luneta took from the daemon's list, or from a go, a go can.
    sure: bool,
    pane: Option<u32>,
    word: Word,
    /// While luneta is not sure of its id: what zellij listed of the session's
    /// clients just before luneta sent its own there, or else what the daemon
    /// listed as luneta's came to it, or first after, if it had not listed
    /// them. Luneta's client is not among them, unless the daemon listed them
    /// after it came.
    before: Option<Others>,
    /// While luneta is not sure of its id: what zellij answered the last look.
    looked: Option<Vec<Client>>,
    /// A session luneta sent its client to, from another, and the pane there,
    /// if it sent it to one.
    going: Option<(String, Option<u32>)>,
    /// Where luneta's client was as luneta sent it to another session, until
    /// zellij lists it there.
    left: Option<Left>,
    /// The number of the last look asked for.
    asked: u64,
    /// The number of the first look asked for in this session: an older one is
    /// of the session the client was in before.
    since: u64,
    /// The number of the newest look taken in.
    taken: u64,
    /// What the daemon listed of every session as it last listed luneta's
    /// client where it is, while luneta is sure of it.
    seen: Option<Vec<Roster>>,
    /// Where luneta's client left with no go of luneta's, until luneta finds
    /// it, or zellij titles luneta's terminal, or luneta's sends it anywhere.
    astray: Option<Astray>,
}

impl OwnClient {
    /// The session luneta's client is in, as far as luneta has followed it.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// The pane luneta's client has the focus on, if luneta knows: none while
    /// it is on a plugin's pane.
    pub fn pane(&self) -> Option<u32> {
        self.pane
    }

    /// The session luneta's client is in and the pane it has the focus on
    /// there, while luneta knows both: the pane's id is that session's. Or
    /// else why luneta does not know the pane.
    pub fn place(&self) -> Result<(&str, u32), Unplaced> {
        let Some(session) = self.session.as_deref() else {
            return Err(Unplaced::Waiting);
        };
        match (self.pane, self.id, &self.looked) {
            (Some(pane), ..) => Ok((session, pane)),
            (None, Some(_), _) => Err(Unplaced::Plugin),
            (None, None, Some(_)) => Err(Unplaced::Unsure),
            (None, None, None) => Err(Unplaced::Waiting),
        }
    }

    /// luneta's client came to `session`, whose clients the daemon `listed`
    /// so, at `now`. What it knew of the session the client was in goes, and
    /// zellij is asked for the clients of this one. Where luneta sent it to a
    /// pane here, that is where it is until zellij says. Back in the session
    /// luneta sent it from, where zellij did not take it out of, it is as it
    /// was there, as the look checks.
    pub fn came(&mut self, session: &str, listed: Option<&[Client]>, now: Instant) -> Look {
        let going = self.going.take().filter(|(to, _)| to == session);
        let went = going.as_ref().and_then(|(_, pane)| *pane);
        let left = self.left.take();
        let back = left.as_ref().filter(|left| left.session == session);
        self.session = Some(session.to_owned());
        self.id = back.map(|left| left.id);
        self.sure = back.is_some_and(|left| left.sure);
        self.pane = back.map_or(went, |left| left.pane);
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        self.before = match back {
            Some(_) => None,
            None => listed.map(|clients| Others { clients: clients.to_vec(), by_zellij: false }),
        };
        self.looked = None;
        self.since = self.asked + 1;
        self.seen = back.and_then(|left| left.seen.clone());
        self.astray = None;
        if going.is_some() {
            self.left = left;
        }
        self.look(went)
    }

    pub fn unfound(&mut self) {
        self.looked = Some(Vec::new());
    }

    /// luneta's client is another from now on, which takes the place of the
    /// one before, here or on another machine: where that one was on its way
    /// to, and the session it left, are no more this one's.
    pub fn anew(&mut self) {
        self.going = None;
        self.left = None;
        self.seen = None;
        self.astray = None;
    }

    /// The session luneta's client left with no go of luneta's, for one luneta
    /// cannot tell, while it cannot.
    pub fn astray(&self) -> Option<&str> {
        self.astray.as_ref().map(|astray| astray.left.as_str())
    }

    /// zellij titled luneta's terminal anew, from the session its client is
    /// in, which the title tells from now on.
    pub fn titled(&mut self) {
        self.astray = None;
    }

    /// luneta sends its client to `pane` of `session`, or to `session` alone,
    /// which the daemon `listed` the clients of so, at `now`. In the session
    /// it is in, the client is on that pane until zellij says, which a look,
    /// once zellij has taken the client there, asks.
    pub fn went(
        &mut self,
        session: &str,
        pane: Option<u32>,
        listed: Option<&[Client]>,
        now: Instant,
    ) -> Option<Look> {
        if self.session.as_deref() != Some(session) {
            self.going = Some((session.to_owned(), pane));
            if let (Some(left), Some(id)) = (self.session.clone(), self.id) {
                let (sure, pane, seen) = (self.sure, self.pane, self.seen.clone());
                self.left = Some(Left { session: left, id, sure, pane, seen });
            }
            return None;
        }
        let pane = pane?;
        self.pane = Some(pane);
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        Some(self.look(Some(pane)))
    }

    /// The daemon lists the clients of each session of the machine so, at
    /// `now`. Once those of the session luneta's client is in are not as they
    /// were when luneta last knew better, or are still so `TRUSTED` after,
    /// they say where its client is, if luneta is sure which it is; or that it
    /// left, where they no longer list it, and luneta sent it nowhere. The
    /// session luneta found it in since, as `hunt` says, if it did.
    pub fn heard(&mut self, rosters: &[Roster], now: Instant) -> Option<String> {
        if self.astray.is_some() {
            return self.hunt(rosters, now);
        }
        let session = self.session.as_deref()?;
        let listed = roster(rosters, session);
        if self.id.is_none() && self.before.is_none() {
            self.before =
                listed.map(|clients| Others { clients: clients.to_vec(), by_zellij: false });
            self.learn(None);
        }
        if let Word::Own { listed: then, until } = &self.word {
            if then.as_deref() == listed && now < *until {
                return None;
            }
            self.word = Word::Daemon;
        }
        let listed = listed?;
        match self.id {
            Some(id) => match listed.iter().find(|client| client.id == id) {
                Some(client) => {
                    self.pane = client.pane;
                    if self.sure {
                        self.seen = Some(rosters.to_vec());
                    }
                },
                None if self.sure && self.going.is_none() => {
                    self.strayed(now);
                    return self.hunt(rosters, now);
                },
                None => {},
            },
            // The pane luneta sent it to, it may have left since.
            None => self.pane = None,
        }
        None
    }

    /// luneta's client, which it is sure of, is no longer in the session luneta
    /// followed it to, at `now`, with no go of luneta's: luneta knows nothing of
    /// it until it finds it, and a look of that session that comes back late
    /// says nothing.
    fn strayed(&mut self, now: Instant) {
        let Some(left) = self.session.take() else {
            return;
        };
        let seen = self.seen.take();
        self.astray = Some(Astray { left, seen, until: now + HUNT });
        self.id = None;
        self.sure = false;
        self.pane = None;
        self.word = Word::Daemon;
        self.before = None;
        self.looked = None;
        self.left = None;
        self.since = self.asked + 1;
    }

    /// Looks for luneta's client, astray, among the clients `rosters` lists at
    /// `now`: it is the one client listed in any session that was not as the
    /// daemon last listed luneta's where it was, while luneta looks. Where
    /// more than one came, or one came to a session it had not listed the
    /// clients of then, luneta cannot tell. The session it found it in,
    /// where it did.
    fn hunt(&mut self, rosters: &[Roster], now: Instant) -> Option<String> {
        let astray = self.astray.as_mut()?;
        let seen = astray.seen.as_deref().filter(|_| now < astray.until)?;
        let (session, own) = match newcomers(seen, rosters).as_deref() {
            Some(&[(session, own)]) => (session.to_owned(), own),
            Some([]) => return None,
            Some(_) | None => {
                astray.seen = None;
                return None;
            },
        };
        self.astray = None;
        self.session = Some(session.clone());
        self.id = Some(own.id);
        self.sure = true;
        self.pane = own.pane;
        self.seen = Some(rosters.to_vec());
        Some(session)
    }

    /// zellij answered a look so, at `now`, while the daemon `listed` the
    /// clients so. One older than a look taken in already, or of a session
    /// the client has left, says nothing. An id luneta took for its own that
    /// zellij does not list, or, unless luneta is sure of it, not on the pane
    /// luneta's client went to, was another's. Whether zellij took no client
    /// of luneta's to that pane, but another, or none: luneta is sure of its
    /// client, zellij lists it elsewhere, and luneta has sent it nowhere
    /// since.
    pub fn looked(&mut self, looked: Looked, listed: Option<&[Client]>, now: Instant) -> bool {
        if looked.number < self.since || looked.number <= self.taken {
            return false;
        }
        self.taken = looked.number;
        let last = looked.number == self.asked;
        let Looked { went, clients, before, .. } = looked;
        if let Some(before) = before {
            // luneta's client came here, and left where it was sent from.
            self.left = None;
            let others = match before {
                Before::Listed(clients) => Some(clients),
                Before::Made => Some(Vec::new()),
                // zellij did not say who was there: what the daemon listed
                // stands.
                Before::Unknown => None,
            };
            if let Some(clients) = others {
                self.before = Some(Others { clients, by_zellij: true });
            }
        }
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        let on = |client: &Client| went.is_none_or(|pane| client.pane == Some(pane));
        let own = self.id.and_then(|id| clients.iter().find(|client| client.id == id));
        if let Some(own) = own.filter(|own| self.sure || on(own)) {
            self.pane = own.pane;
            return last && !on(own);
        }
        if own.is_none() && self.sure && self.going.is_none() {
            self.strayed(now);
            return false;
        }
        self.id = None;
        self.sure = false;
        self.looked = Some(clients);
        self.pane = went;
        self.learn(went);
        false
    }

    /// Takes the one client that can be luneta's for its own, unless luneta
    /// is sure of it already: of what zellij answered the last look, the one
    /// it listed, or else the one the daemon did not list before luneta's
    /// client came to the session, or else the one alone on the pane luneta's
    /// client `went` to. The pane is that client's, as zellij answered.
    fn learn(&mut self, went: Option<u32>) {
        let (None, Some(looked)) = (self.id, &self.looked) else {
            return;
        };
        let before = self.before.as_ref();
        let new = |client: &&Client| {
            before.is_some_and(|before| before.clients.iter().all(|was| was.id != client.id))
        };
        let there = |client: &&Client| went.is_some_and(|pane| client.pane == Some(pane));
        let by_zellij = before.is_some_and(|before| before.by_zellij);
        let Some((own, sure)) = alone(looked.iter())
            .map(|own| (own, true))
            .or_else(|| alone(looked.iter().filter(new)).map(|own| (own, by_zellij)))
            .or_else(|| alone(looked.iter().filter(there)).map(|own| (own, false)))
        else {
            return;
        };
        self.id = Some(own.id);
        self.sure = sure;
        self.pane = own.pane;
        self.before = None;
        self.looked = None;
    }

    /// A look at the session luneta's client is in, if luneta knows which,
    /// and sends it to no other: a client luneta is sure of that zellij does
    /// not list there left it.
    pub fn again(&mut self) -> Option<Look> {
        if self.session.is_none() || self.going.is_some() {
            return None;
        }
        Some(self.look(None))
    }

    fn look(&mut self, went: Option<u32>) -> Look {
        self.asked += 1;
        let session = self.session.clone().expect("luneta looks at the session its client is in");
        Look { session, number: self.asked, went }
    }
}

/// The one client of `clients`, if there is exactly one.
fn alone<'a>(mut clients: impl Iterator<Item = &'a Client>) -> Option<Client> {
    match (clients.next(), clients.next()) {
        (Some(client), None) => Some(*client),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: u32, pane: u32) -> Client {
        Client { id, pane: Some(pane) }
    }

    fn listed(session: &str, clients: &[Client]) -> Roster {
        Roster { session: session.to_owned(), clients: Some(clients.to_vec()) }
    }

    /// The daemon lists one session, `work`, with these clients.
    fn work(clients: &[Client]) -> Vec<Roster> {
        vec![listed("work", clients)]
    }

    /// What zellij answers `look`, having taken luneta's client where the look
    /// says it went.
    fn answer(look: &Look, clients: &[Client]) -> Looked {
        Looked { number: look.number, went: look.went, clients: clients.to_vec(), before: None }
    }

    /// luneta's client came to the session alone, and so is the one zellij
    /// lists. A plain `zellij attach` that comes after it, and goes elsewhere,
    /// takes the mark nowhere.
    #[test]
    fn a_second_client_never_moves_the_mark() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        assert_eq!(own.pane(), Some(3));
        own.heard(&work(&[client(1, 5), client(2, 5)]), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&[client(1, 5), client(2, 7)]), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&[client(1, 6), client(2, 7)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    /// luneta's client came to a session another was in already: it is the
    /// one the daemon did not list before, or that zellij did not list just
    /// before luneta sent its client there, as the daemon may not have yet.
    /// Where zellij could not list them, what the daemon listed stands.
    /// zellij put it on the pane the other has the focus on.
    #[test]
    fn luneta_s_client_is_the_one_that_came_with_it() {
        let now = Instant::now();
        let other = [client(1, 3)];
        let by_zellij = Some(Before::Listed(other.to_vec()));
        let unknown = Some(Before::Unknown);
        let cases = [(Some(&other[..]), None), (None, by_zellij), (Some(&other[..]), unknown)];
        for (listed, before) in cases {
            let mut own = OwnClient::default();
            let look = own.came("work", listed, now);
            let both = [client(1, 3), client(2, 3)];
            own.looked(Looked { before: before.clone(), ..answer(&look, &both) }, listed, now);
            own.heard(&work(&[client(1, 4), client(2, 3)]), now);
            assert_eq!(own.pane(), Some(3), "{listed:?} {before:?}");
            own.heard(&work(&[client(1, 4), client(2, 6)]), now);
            assert_eq!(own.pane(), Some(6), "{listed:?} {before:?}");
        }
    }

    /// luneta's client left the session and came back before the daemon
    /// listed its clients again: the daemon lists it where it was, under the
    /// id zellij gave it again, until it lists them anew.
    #[test]
    fn the_daemon_s_list_from_before_a_look_does_not_undo_it() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale), now);
        own.looked(answer(&look, &[client(1, 5)]), Some(&stale), now);
        own.heard(&work(&stale), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&[client(1, 4)]), now);
        assert_eq!(own.pane(), Some(4));
    }

    /// The sidebar sends luneta's client to a pane, and zellij's keys take it
    /// back where it was before the daemon lists the clients again. The
    /// daemon then lists them as it did before the go, which, once it has had
    /// the time to list them anew, says where the client is.
    #[test]
    fn the_daemon_s_list_says_where_the_client_is_once_it_is_newer_than_a_look() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let listed = [client(1, 3)];
        own.heard(&work(&listed), now);
        let went = own.went("work", Some(5), Some(&listed), now).unwrap();
        own.looked(answer(&went, &[client(1, 5)]), Some(&listed), now);
        own.heard(&work(&listed), now + TRUSTED / 2);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&listed), now + TRUSTED);
        assert_eq!(own.pane(), Some(3));
    }

    /// The look after a go comes back before the one asked as the client came
    /// to the session, and one of a session the client has left comes back
    /// once it is in another.
    #[test]
    fn a_look_that_comes_back_late_says_nothing() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let came = own.came("work", None, now);
        let went = own.went("work", Some(4), None, now).unwrap();
        own.looked(answer(&went, &[client(1, 4)]), None, now);
        own.looked(answer(&came, &[client(1, 3)]), None, now);
        assert_eq!(own.pane(), Some(4));

        let went = own.went("work", Some(5), None, now).unwrap();
        own.came("play", None, now);
        own.looked(answer(&went, &[client(1, 5)]), None, now);
        assert_eq!(own.pane(), None);
    }

    /// Two clients came to the session as the daemon last listed none: the
    /// one alone on the pane luneta then sent its client to is luneta's. A go
    /// zellij did not take the client on tells nothing, and the pane it was
    /// sent to is not where it is.
    #[test]
    fn clients_that_came_together_are_told_apart_by_a_go() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.pane(), None);

        let failed = own.went("work", Some(4), None, now).unwrap();
        assert_eq!(own.pane(), Some(4));
        let both = [client(1, 3), client(2, 4)];
        own.looked(Looked { went: None, ..answer(&failed, &both) }, None, now);
        assert_eq!(own.pane(), None);

        let went = own.went("work", Some(3), None, now).unwrap();
        own.looked(answer(&went, &[client(1, 3), client(2, 4)]), None, now);
        own.heard(&work(&[client(1, 6), client(2, 4)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    /// The daemon's list from before luneta's client came missed a client
    /// that came just before it, which luneta took for its own: the client
    /// alone on the pane luneta sends its own to is luneta's.
    #[test]
    fn a_go_takes_back_an_id_that_was_another_s() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale), now);
        own.looked(answer(&look, &[client(1, 3), client(3, 4)]), Some(&stale), now);
        assert_eq!(own.pane(), Some(4));
        let went = own.went("work", Some(5), Some(&stale), now).unwrap();
        own.looked(answer(&went, &[client(1, 5), client(3, 4)]), Some(&stale), now);
        own.heard(&work(&[client(1, 6), client(3, 4)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    /// luneta is sure of its client, which zellij listed alone. A go that
    /// zellij takes another client on, as one that typed in it last, leaves
    /// luneta's client where it is, which the look says, and the other never
    /// moves the mark. A go zellij takes luneta's client on says nothing, nor
    /// does the look of one luneta sent it on again before it came back.
    #[test]
    fn a_go_that_took_another_client_keeps_an_id_luneta_is_sure_of() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let listed = [client(1, 3), client(2, 4)];
        own.heard(&work(&listed), now);
        let went = own.went("work", Some(5), Some(&listed), now).unwrap();
        assert!(own.looked(answer(&went, &[client(1, 3), client(2, 5)]), Some(&listed), now));
        assert_eq!(own.pane(), Some(3));
        own.heard(&work(&[client(1, 3), client(2, 6)]), now);
        assert_eq!(own.pane(), Some(3));
        own.heard(&work(&[client(1, 7), client(2, 6)]), now);
        assert_eq!(own.pane(), Some(7));

        let went = own.went("work", Some(8), None, now).unwrap();
        let again = own.went("work", Some(9), None, now).unwrap();
        let there = [client(1, 9), client(2, 6)];
        assert!(!own.looked(answer(&went, &there), None, now));
        assert!(!own.looked(answer(&again, &there), None, now));
        assert_eq!(own.pane(), Some(9));
    }

    /// Sent to a pane of another session, luneta's client is there as soon as
    /// it comes to the session, before zellij says.
    #[test]
    fn a_go_to_another_session_marks_its_pane_at_once() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        own.came("work", None, now);
        assert_eq!(own.went("play", Some(7), None, now), None);
        let look = own.came("play", Some(&[client(1, 2)]), now);
        assert_eq!((own.pane(), look.went), (Some(7), Some(7)));
    }

    /// luneta says why it does not know the pane its client is on: it waits
    /// on zellij, or cannot tell its client from another that came with it
    /// until it sends it to a pane, or the client is on a plugin's pane.
    #[test]
    fn luneta_says_why_it_does_not_know_the_pane() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        assert_eq!(own.place(), Err(Unplaced::Waiting));
        let look = own.came("work", None, now);
        assert_eq!(own.place(), Err(Unplaced::Waiting));
        own.looked(answer(&look, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.place(), Err(Unplaced::Unsure));
        let went = own.went("work", Some(4), None, now).unwrap();
        assert_eq!(own.place(), Ok(("work", 4)));
        own.looked(answer(&went, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.place(), Ok(("work", 4)));
        own.heard(&work(&[client(1, 3), Client { id: 2, pane: None }]), now + TRUSTED);
        assert_eq!(own.place(), Err(Unplaced::Plugin));
    }

    /// luneta knows its client among two, and sends it to another session,
    /// which zellij does not take it to. Back in the session it never left,
    /// it is the client it was, which the look finds where it was.
    #[test]
    fn a_go_zellij_did_not_take_the_client_on_keeps_its_id() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let other = [client(1, 3)];
        let look = own.came("work", None, now);
        let both = [client(1, 3), client(2, 3)];
        let before = Some(Before::Listed(other.to_vec()));
        own.looked(Looked { before, ..answer(&look, &both) }, None, now);
        let listed = [client(1, 3), client(2, 4)];
        own.heard(&work(&listed), now);
        assert_eq!(own.went("play", Some(7), None, now), None);
        own.came("play", None, now);
        let look = own.came("work", Some(&listed), now);
        assert_eq!(own.pane(), Some(4));
        own.looked(answer(&look, &listed), Some(&listed), now);
        assert_eq!(own.pane(), Some(4));
        own.heard(&work(&[client(1, 6), client(2, 4)]), now);
        assert_eq!(own.pane(), Some(4));
    }

    /// luneta is sure of its client, alone in `work`, and the daemon lists
    /// another in `play`.
    fn sure_in_work(now: Instant) -> OwnClient {
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let rosters = [listed("work", &[client(1, 3)]), listed("play", &[client(1, 2)])];
        assert_eq!(own.heard(&rosters, now + TRUSTED), None);
        own
    }

    /// Something other than luneta takes its client from `work` to `play`,
    /// where zellij gives it a new id, and the daemon lists `work` first. Until
    /// it lists `play` again, luneta does not know where its client is, and
    /// then it is the one that came there.
    #[test]
    fn a_client_moved_by_another_is_the_one_that_came_to_any_session() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let left = [listed("work", &[]), listed("play", &[client(1, 2)])];
        assert_eq!(own.heard(&left, now + TRUSTED), None);
        assert_eq!((own.astray(), own.session()), (Some("work"), None));
        assert_eq!(own.place(), Err(Unplaced::Waiting));
        let came = [listed("work", &[]), listed("play", &[client(1, 2), client(2, 5)])];
        assert_eq!(own.heard(&came, now + TRUSTED), Some("play".to_owned()));
        assert_eq!((own.astray(), own.place()), (None, Ok(("play", 5))));
        own.heard(&[listed("work", &[]), listed("play", &[client(1, 2), client(2, 6)])], now);
        assert_eq!(own.pane(), Some(6));
    }

    /// zellij no longer lists luneta's client in the session it was in, as the
    /// look after a go that zellij took it nowhere on finds, and the daemon
    /// lists it in a session it made since.
    #[test]
    fn a_look_that_does_not_find_the_client_sets_luneta_looking_for_it() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let went = own.went("work", Some(4), None, now).unwrap();
        assert!(!own.looked(answer(&went, &[]), None, now));
        assert_eq!(own.astray(), Some("work"));
        let play = listed("play", &[client(1, 2)]);
        let made = [listed("work", &[]), play, listed("new", &[client(1, 0)])];
        assert_eq!(own.heard(&made, now), Some("new".to_owned()));
        assert_eq!(own.place(), Ok(("new", 0)));
    }

    /// Two clients came as luneta's left, and luneta cannot tell which is its
    /// own, nor one that comes after, nor one that comes once it has looked
    /// for as long as it does. A title of zellij's says where it is.
    #[test]
    fn luneta_takes_no_client_for_its_own_where_it_cannot_tell() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let play = listed("play", &[client(1, 2), client(2, 5), client(3, 5)]);
        let two = [listed("work", &[]), play];
        assert_eq!(own.heard(&two, now + TRUSTED), None);
        let one = [listed("work", &[]), listed("play", &[client(1, 2), client(2, 5)])];
        assert_eq!(own.heard(&one, now + TRUSTED), None);
        assert_eq!(own.astray(), Some("work"));

        let mut own = sure_in_work(now);
        own.heard(&[listed("work", &[]), listed("play", &[client(1, 2)])], now + TRUSTED);
        let late = [listed("work", &[]), listed("play", &[client(1, 2), client(2, 5)])];
        assert_eq!(own.heard(&late, now + TRUSTED + HUNT), None);
        assert_eq!(own.astray(), Some("work"));
        own.titled();
        assert_eq!(own.astray(), None);
    }

    /// The daemon had yet to list the clients of `play` as it last listed
    /// luneta's client in `work`: one it lists there since may be anyone's,
    /// and luneta cannot tell. Where it lists none there, the one that came
    /// to another session is luneta's.
    #[test]
    fn a_session_whose_clients_were_not_listed_hides_none() {
        let now = Instant::now();
        let unlisted = Roster { session: "play".to_owned(), clients: None };
        for play in [listed("play", &[client(1, 2)]), unlisted.clone()] {
            let mut own = OwnClient::default();
            let look = own.came("work", None, now);
            own.looked(answer(&look, &[client(1, 3)]), None, now);
            own.heard(&[listed("work", &[client(1, 3)]), unlisted.clone()], now + TRUSTED);
            let left = [listed("work", &[]), play.clone()];
            own.heard(&left, now + TRUSTED);
            let came = [listed("work", &[]), play.clone(), listed("other", &[client(1, 4)])];
            let found = own.heard(&came, now + TRUSTED);
            match play.clients {
                Some(_) => assert_eq!((found, own.astray()), (None, Some("work"))),
                None => assert_eq!(own.place(), Ok(("other", 4))),
            }
        }
    }

    /// luneta sends its client from `work` to `play`, and the daemon lists
    /// `work` without it before luneta's client comes to `play`: it is on its
    /// way, not astray.
    #[test]
    fn a_client_luneta_sends_elsewhere_is_not_astray() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        assert_eq!(own.went("play", None, None, now), None);
        own.heard(&[listed("work", &[]), listed("play", &[client(1, 2)])], now + TRUSTED);
        assert_eq!((own.astray(), own.session()), (None, Some("work")));
    }
}
