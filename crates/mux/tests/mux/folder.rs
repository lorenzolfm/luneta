use std::process::Command;

use crate::host::LUNETA_MUX;

#[test]
fn it_listens_only_in_the_one_absolute_folder_it_is_given() {
    let refused: [(&[&str], &str); 3] = [
        (&[], "give it the one folder to listen in"),
        (&["/tmp/a", "/tmp/b"], "give it the one folder to listen in"),
        (&["luneta"], "luneta is no folder to listen in, as it is not absolute"),
    ];
    for (arguments, why) in refused {
        let output = Command::new(LUNETA_MUX).args(arguments).output().unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains(why), "{arguments:?}: {said}");
    }
}
