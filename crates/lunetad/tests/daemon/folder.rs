use std::fs::{self, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use crate::host::{info, start, Host, Vars, LUNETA, SOCKET};

type Case = for<'p> fn(&'p Path, &'p Path) -> (Vars<'p>, &'p Path);

#[test]
fn the_cli_finds_the_daemon_where_the_variables_say() {
    let cases: [Case; 2] = [
        |a, _| (Vars { runtime: Some(a), ..Vars::default() }, a),
        |a, b| (Vars { runtime: Some(a), temp: Some(b), ..Vars::default() }, a),
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

#[test]
fn a_second_lunetad_of_one_home_names_the_first_and_does_not_start() {
    let host = Host::new();
    let _first = start(host.vars()).unwrap();
    let other = tempfile::tempdir().unwrap();
    let (ended, said) = start(Vars { runtime: Some(other.path()), ..host.vars() }).err().unwrap();
    assert!(!ended.success());
    let first = host.runtime.path().join("luneta");
    let names = format!("and listens in {}: `luneta terminate` ends it", first.display());
    assert!(said.contains(&names), "{said}");
    assert!(!other.path().join(SOCKET).exists(), "a second lunetad of one home listens");
}

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

#[test]
fn the_cli_refuses_a_folder_that_is_not_yours_alone() {
    let root = tempfile::tempdir().unwrap();
    let vars = Vars::runtime(root.path());
    let _daemon = start(vars).unwrap();
    fs::set_permissions(root.path().join("luneta"), Permissions::from_mode(0o755)).unwrap();

    let output = vars.command(LUNETA).arg("info").output().unwrap();
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("is open to other users"), "{said}");
}
