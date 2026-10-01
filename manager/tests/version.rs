//! `bana-manager version`: what a release's bin/bana compares with its own.

use std::process::Command;

#[test]
fn version_subcommand_prints_pkg_version() {
    let o = Command::new(env!("CARGO_BIN_EXE_bana-manager"))
        .arg("version")
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );
}
