use std::process::Stdio;

use assert_cmd::prelude::*;
use docker_utils::{DockerCompose, ProjectId, Service, User};
use predicates::prelude::*;

#[test]
fn exit_code() {
    let project_id = ProjectId::new(env!("CARGO_PKG_NAME"));
    let mut compose = DockerCompose::up(project_id);

    let server = compose
        .exec(
            Service::Client,
            User::Sle,
            ["ssh-local-exec-server", "--listen", "localhost:22222"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    compose
        .exec(Service::Client, User::Sle, ["wait-for", "localhost:22222"])
        .assert()
        .success();

    // success
    compose
        .exec(
            Service::Client,
            User::Sle,
            [
                "ssh-local-exec",
                "--connect",
                "localhost:22222",
                "--",
                "bash",
                "-c",
                "exit 0",
            ],
        )
        .assert()
        .stderr(predicate::str::contains("code: 0"))
        .code(predicate::eq(0));

    // exit with 33
    compose
        .exec(
            Service::Client,
            User::Sle,
            [
                "ssh-local-exec",
                "--connect",
                "localhost:22222",
                "--",
                "bash",
                "-c",
                "exit 33",
            ],
        )
        .assert()
        .stderr(predicate::str::contains("code: 33"))
        .code(predicate::eq(33));

    // exit with signal 15
    compose
        .exec(
            Service::Client,
            User::Sle,
            [
                "ssh-local-exec",
                "--connect",
                "localhost:22222",
                "--",
                "bash",
                "-c",
                "kill -15 $$",
            ],
        )
        .assert()
        .stderr(predicate::str::contains("signal: 15"))
        .code(predicate::eq(128 + 15));

    // exit with spawn failure
    compose
        .exec(
            Service::Client,
            User::Sle,
            [
                "ssh-local-exec",
                "--connect",
                "localhost:22222",
                "--",
                "not_exist_command",
            ],
        )
        .assert()
        .stderr(predicate::str::contains("No such file or directory"))
        .code(predicate::eq(255));

    compose
        .exec(
            Service::Client,
            User::Sle,
            ["killall", "ssh-local-exec-server"],
        )
        .assert()
        .success();

    server.wait_with_output().unwrap().assert().success();

    compose.down();
}
