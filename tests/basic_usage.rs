use std::process::Command;

use assert_cmd::prelude::*;
use docker_utils::{DockerCompose, ProjectId, Service, User};
use predicates::prelude::*;

#[test]
fn show_help() {
    let command_names = [
        "git-credential-ssh-local",
        "ssh-local-exec-server",
        "ssh-local-exec",
    ];

    for command_name in command_names {
        Command::new("cargo")
            .args(["run", "--bin", command_name, "--", "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Usage:").and(predicate::str::contains(command_name)));
    }
}

#[test]
fn compose_sanity_check() {
    let project_id = ProjectId::new(env!("CARGO_PKG_NAME"));
    let mut compose = DockerCompose::up(project_id);

    // check hostname is expected
    compose
        .exec(Service::Server, User::Sle, ["cat", "/etc/hostname"])
        .assert()
        .stdout(predicate::eq("server\n"))
        .success();
    compose
        .exec(Service::Client, User::Sle, ["cat", "/etc/hostname"])
        .assert()
        .stdout(predicate::eq("client\n"))
        .success();

    compose.down();
}
