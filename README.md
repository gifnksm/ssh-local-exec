<!-- cargo-sync-rdme title [[ -->
# ssh-local-exec
<!-- cargo-sync-rdme ]] -->
<!-- cargo-sync-rdme badge [[ -->
[![Maintenance: experimental](https://img.shields.io/badge/maintenance-experimental-blue.svg?style=flat-square)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-badges-section)
[![License: MIT or Apache-2.0](https://img.shields.io/crates/l/ssh-local-exec.svg?style=flat-square)](#license)
[![crates.io](https://img.shields.io/crates/v/ssh-local-exec.svg?logo=rust&style=flat-square)](https://crates.io/crates/ssh-local-exec)
[![docs.rs](https://img.shields.io/docsrs/ssh-local-exec.svg?logo=docs.rs&style=flat-square)](https://docs.rs/ssh-local-exec)
[![Rust: ^1.64.0](https://img.shields.io/badge/rust-^1.64.0-93450a.svg?logo=rust&style=flat-square)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
[![GitHub Actions: Rust CI](https://img.shields.io/github/actions/workflow/status/gifnksm/ssh-local-exec/rust-ci.yml.svg?label=Rust+CI&logo=github&style=flat-square)](https://github.com/gifnksm/ssh-local-exec/actions/workflows/rust-ci.yml)
[![Codecov](https://img.shields.io/codecov/c/github/gifnksm/ssh-local-exec.svg?label=codecov&logo=codecov&style=flat-square)](https://codecov.io/gh/gifnksm/ssh-local-exec)
<!-- cargo-sync-rdme ]] -->

ssh-local-exec is a simple command line utility that provides a way to execute a command on the SSH local host from the remote host.

```console
# Launch ssh-local-exec server
$ ssh-local-exec-server --local-endpoint localhost:22222

# Log in to the remote host with the reverse port forwarding
$ ssh -R 33333:localhost:22222 remote-host

# Execute a command on the local host from the remote host
$ ssh-local-exec --remote-endpoint localhost:33333 -- hostname
# => local-host
```

ssh-local-exec also provides a git-credential-helper that can be used to forward the authentication information on the local host to the remote host.

## Installation

There are multiple ways to install ssh-local-exec.
Choose any one of the methods below that best suits your needs.

### Pre-built binaries

Executable binaries are available for download on the [GitHub Release page].

[GitHub Release page]: https://github.com/gifnksm/ssh-local-exec/releases/

### Build from source using Rust

To build ssh-local-exec executable from the source, you must have the Rust toolchain installed.
To install the rust toolchain, follow [this guide](https://www.rust-lang.org/tools/install).

Once you have installed Rust, the following command can be used to build and install ssh-local-exec:

```console
# Install released version
$ cargo install ssh-local-exec

# Install latest version
$ cargo install --git https://github.com/gifnksm/ssh-local-exec.git ssh-local-exec
```

## Minimum supported Rust version (MSRV)

The minimum supported Rust version is **Rust 1.64.0**.
At least the last 3 versions of stable Rust are supported at any given time.

While a crate is a pre-release status (0.x.x) it may have its MSRV bumped in a patch release.
Once a crate has reached 1.x, any MSRV bump will be accompanied by a new minor version.

## License

This project is licensed under either of

* Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license
   ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

## Contribution

Unless you explicitly state otherwise, any contributions intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

See [CONTRIBUTING.md](CONTRIBUTING.md).
