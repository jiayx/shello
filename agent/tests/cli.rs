use std::process::Command;

fn agent() -> Command {
    Command::new(env!("CARGO_BIN_EXE_shello-agent"))
}

#[test]
fn prints_the_packaged_version() {
    let output = agent().arg("--version").output().unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("shello-agent {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn reports_invalid_arguments() {
    let output = agent().arg("--unknown").output().unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("shello-agent: unknown argument: --unknown"));
}
