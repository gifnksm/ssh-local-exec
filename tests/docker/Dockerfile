#syntax=docker/dockerfile:1

FROM archlinux:base-devel

# Set default shell for building image
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN \
  --mount=type=cache,target=/var/cache \
  <<END
pacman -Syu --noconfirm
pacman -S jq openssh rsync rustup --noconfirm

useradd -m sle
mkdir /work
chown sle:sle /work

systemctl enable sshd.service
cat <<SSH_END >> /etc/ssh/ssh_config
Host *
  StrictHostKeyChecking no
SSH_END
END

USER sle

RUN <<END
mkdir -m 700 ~/.ssh
ssh-keygen -t rsa -b 4096 -f ~/.ssh/id_rsa -q -N ""
touch ~/.ssh/authorized_keys
cat ~/.ssh/id_rsa.pub >> ~/.ssh/authorized_keys

rustup install stable --profile=minimal
END

WORKDIR /work
COPY --chown=sle:sle . /work/

RUN \
  --mount=type=cache,uid=1000,gid=1000,target=/home/sle/.cargo/registry \
  <<END
cargo build
END

ENV PATH="/work/target/debug/:$PATH"

# hadolint ignore=DL3002
USER root

CMD ["/sbin/init"]

# Set default shell for running container
SHELL ["/bin/bash", "-c"]
