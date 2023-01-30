use std::process::{Command, Stdio};

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

#[test]
fn execute_command() {
    let project_id = ProjectId::new(env!("CARGO_PKG_NAME"));
    let mut compose = DockerCompose::up(project_id);

    let server = compose
        .exec(
            Service::Server,
            User::Sle,
            ["ssh-local-exec-server", "--listen", "server:22222"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    compose
        .exec(Service::Client, User::Sle, ["wait-for", "server:22222"])
        .assert()
        .success();

    compose
        .exec(
            Service::Client,
            User::Sle,
            [
                "ssh-local-exec",
                "--connect",
                "server:22222",
                "cat",
                "/etc/hostname",
            ],
        )
        .assert()
        .stdout(predicate::eq("server\n"))
        .success();

    compose
        .exec(
            Service::Server,
            User::Sle,
            ["killall", "ssh-local-exec-server"],
        )
        .assert()
        .success();

    server.wait_with_output().unwrap().assert().success();

    compose.down();
}

#[test]
fn execute_command_via_ssh() {
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
    compose
        .exec(Service::Server, User::Sle, ["wait-for", "localhost:22"])
        .assert()
        .success();

    compose
        .exec(
            Service::Client,
            User::Sle,
            [
                "ssh",
                "server",
                "-R",
                "localhost:33333:localhost:22222",
                "ssh-local-exec",
                "--connect",
                "localhost:33333",
                "cat",
                "/etc/hostname",
            ],
        )
        .assert()
        .stdout(predicate::eq("client\n"))
        .success();

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
