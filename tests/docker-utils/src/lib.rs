use assert_cmd::prelude::*;
use std::{ffi::OsStr, fmt, process::Command, sync::Once};

#[derive(Debug)]
pub struct ProjectId {
    unique_name: &'static str,
    pid: u32,
    seq_no: u32,
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-test-{}-{}", self.unique_name, self.pid, self.seq_no)
    }
}

impl ProjectId {
    pub fn new(unique_name: &'static str) -> Self {
        use std::{
            process,
            sync::atomic::{AtomicU32, Ordering},
        };

        static SEQ_NO: AtomicU32 = AtomicU32::new(0);
        let pid = process::id();
        let seq_no = SEQ_NO.fetch_add(1, Ordering::SeqCst);
        Self {
            unique_name,
            pid,
            seq_no,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Server,
    Client,
}

impl Service {
    const ALL: &[Service] = &[Self::Server, Self::Client];

    fn as_str(&self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
        }
    }
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum User {
    Root,
    Sle,
}

impl User {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Sle => "sle",
        }
    }
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

#[derive(Debug)]
pub struct DockerCompose {
    project_id: ProjectId,
    finalized: bool,
}

impl DockerCompose {
    fn compose(project_id: Option<&ProjectId>) -> Command {
        static DOCKER_COMPOSE_FILE: &str =
            concat!(env!("CARGO_MANIFEST_DIR"), "/../docker/docker-compose.yml");
        #[cfg(not(coverage))]
        static ENV_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../docker/.env.debug");
        #[cfg(coverage)]
        static ENV_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../docker/.env.coverage");

        let mut cmd = Command::new("docker");
        cmd.args(["compose", "-f", DOCKER_COMPOSE_FILE, "--env-file", ENV_FILE]);
        if let Some(project_id) = project_id {
            cmd.args(["-p", &project_id.to_string()]);
        }
        cmd
    }

    fn build() {
        Self::compose(None).args(["build"]).assert().success();
    }

    pub fn up(project_id: ProjectId) -> Self {
        static BUILD: Once = Once::new();
        BUILD.call_once(Self::build);

        Self::compose(Some(&project_id))
            .args(["up", "-d"])
            .assert()
            .success();
        Self {
            project_id,
            finalized: false,
        }
    }

    pub fn down(&mut self) {
        if !self.finalized {
            self.finalized = true;
            for service in Service::ALL {
                self.exec(*service, User::Sle, ["/opt/ssh-local-exec/finalize"])
                    .assert()
                    .success();
            }
            #[cfg(coverage)]
            {
                for service in Service::ALL {
                    Self::compose(Some(&self.project_id))
                        .args([
                            "cp",
                            &format!("{service}:/work/target/lcov/"),
                            &format!("lcov.{}.{}", self.project_id, service),
                        ])
                        .assert()
                        .success();
                }
            }
        }

        Self::compose(Some(&self.project_id))
            .args(["down", "--volumes"])
            .assert()
            .success();
    }

    pub fn exec<I, S>(&self, service: Service, user: User, command: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Self::compose(Some(&self.project_id));
        cmd.args(["exec", "-u", user.as_str(), service.as_str()])
            .args(command);
        cmd
    }

    pub fn exec_detatched<I, S>(&self, service: Service, user: User, command: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Self::compose(Some(&self.project_id));
        cmd.args(["exec", "-u", user.as_str(), "-d", service.as_str()])
            .args(command);
        cmd
    }
}

impl Drop for DockerCompose {
    fn drop(&mut self) {
        self.down();
    }
}
