#syntax=docker/dockerfile:1

FROM archlinux:base-devel

ARG BUILD_TYPE=debug

# Set default shell for building image
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

ADD https://raw.githubusercontent.com/eficode/wait-for/v2.2.3/wait-for /usr/local/bin/wait-for

RUN \
  --mount=type=cache,target=/var/cache \
  <<END
pacman -Syu --noconfirm
pacman -S jq openssh rsync rustup gnu-netcat wget --noconfirm

curl -LsSf "https://github.com/taiki-e/cargo-llvm-cov/releases/latest/download/cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz" | tar xzf - -C /usr/local/bin/

chmod +rx /usr/local/bin/wait-for

useradd -m sle
mkdir /work
chown sle:sle /work

systemctl enable sshd.service
cat <<SSH_END >> /etc/ssh/ssh_config
Host *
  StrictHostKeyChecking no
SSH_END
END

COPY --chown=sle:sle ./tests/docker/assets/ /opt/ssh-local-exec/
RUN <<END
echo -n "${BUILD_TYPE}" > /opt/ssh-local-exec/build-type
ln -s /opt/ssh-local-exec/run-command /usr/local/bin/ssh-local-exec
ln -s /opt/ssh-local-exec/run-command /usr/local/bin/ssh-local-exec-server
ln -s /opt/ssh-local-exec/run-command /usr/local/bin/git-credential-ssh-local
END


USER sle

RUN <<END
mkdir -m 700 ~/.ssh
ssh-keygen -t rsa -b 4096 -f ~/.ssh/id_rsa -q -N ""
touch ~/.ssh/authorized_keys
cat ~/.ssh/id_rsa.pub >> ~/.ssh/authorized_keys

rustup install stable --profile=minimal
rustup component add llvm-tools
mkdir -p ~/.cargo/registry
END

WORKDIR /work
COPY --chown=sle:sle . /work/

RUN \
  --mount=type=cache,uid=1000,gid=1000,target=/home/sle/.cargo/registry \
  <<END
/opt/ssh-local-exec/build
END


# hadolint ignore=DL3002
USER root

CMD ["/sbin/init"]

# Set default shell for running container
SHELL ["/bin/bash", "-c"]
