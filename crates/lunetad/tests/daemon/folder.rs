//! Where the daemon listens, and that it is the only one there.

use std::fs::{self, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use crate::host::{info, start, Vars, SOCKET};

/// A case of where the socket goes: given two roots, the variables to start a
/// daemon with, and the root that should then hold the socket.
type Case = for<'p> fn(&'p Path, &'p Path) -> (Vars<'p>, &'p Path);

/// The daemon listens in `luneta` under `$XDG_RUNTIME_DIR`, or under `$TMPDIR`
/// where that is unset or set to nothing, as on macOS, and the CLI looks for
/// it there by the same rule. The shell hook goes by it too.
#[test]
fn the_cli_finds_the_daemon_where_the_variables_say() {
    let cases: [Case; 4] = [
        |a, _| (Vars { runtime: Some(a), ..Vars::default() }, a),
        |_, b| (Vars { temp: Some(b), ..Vars::default() }, b),
        |a, b| (Vars { runtime: Some(a), temp: Some(b), ..Vars::default() }, a),
        |_, b| (Vars { runtime: Some(Path::new("")), temp: Some(b), ..Vars::default() }, b),
    ];
    for case in cases {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (vars, root) = case(a.path(), b.path());
        let daemon = start(vars).unwrap();
        assert!(root.join(SOCKET).exists(), "no socket under {}", root.display());
        let said = info(vars);
        assert!(said.contains(&format!("(pid {})", daemon.pid())), "{said}");
    }
}

/// One daemon runs in a folder at a time. The lock that says so goes with the
/// process however it ends, so that a daemon that crashed and left its socket
/// behind does not keep the next one from starting.
#[test]
fn one_daemon_holds_the_folder_until_it_dies() {
    let root = tempfile::tempdir().unwrap();
    let vars = Vars::runtime(root.path());
    let first = start(vars).unwrap();

    let (ended, said) = start(vars).err().unwrap();
    assert!(!ended.success());
    assert!(said.contains("already runs"), "{said}");
    let said = info(vars);
    assert!(said.contains(&format!("(pid {})", first.pid())), "{said}");

    drop(first);
    assert!(root.path().join(SOCKET).exists(), "a crash leaves the socket behind");
    let next = start(vars).unwrap();
    let said = info(vars);
    assert!(said.contains(&format!("(pid {})", next.pid())), "{said}");
}

/// A folder that is a link, or that other users can reach into, is refused:
/// in a `TMPDIR` of `/tmp`, anyone could have made it first.
#[test]
fn the_daemon_refuses_a_folder_that_is_not_yours_alone() {
    let (linked, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    symlink(elsewhere.path(), linked.path().join("luneta")).unwrap();
    let open = tempfile::tempdir().unwrap();
    fs::create_dir(open.path().join("luneta")).unwrap();
    fs::set_permissions(open.path().join("luneta"), Permissions::from_mode(0o755)).unwrap();

    for (root, why) in [
        (linked.path(), "is not a folder of yours alone"),
        (open.path(), "is open to other users"),
    ] {
        let (ended, said) = start(Vars::runtime(root)).err().unwrap();
        assert!(!ended.success());
        assert!(said.contains(why), "{said}");
    }
    assert!(!elsewhere.path().join("lunetad.sock").exists());
    assert!(!open.path().join(SOCKET).exists());
}
