# ezra

A container that runs Claude Code and Codex for the Claude and ChatGPT phone apps, managed from a web page on port 8443.

> [!NOTE]
> Most of the code is AI-generated. The agent CLIs and the AI space around them change fast and this project may not be useful a year from now, so I don't invest time to review every line by hand. I plan each change with agents, skim the code, and run the unit and end-to-end tests before committing.

## Run it

```yaml
services:
  ezra:
    image: tastypackets/ezra:latest
    hostname: my-box # the name the Claude app shows
    ports:
      - "8443:8443"
    volumes:
      - config:/config
      - projects:/home/dev/projects
      - cache:/cache
    stop_grace_period: 60s
    # Codex's sandbox needs these for the ChatGPT app's folder picker.
    # Remove them if you only use Claude Code.
    security_opt:
      - seccomp=unconfined
      - apparmor=unconfined

volumes:
  config:
  projects:
  cache:
```

| Volume | Holds |
|---|---|
| `/config` | The manager password, agent sign-ins, settings and chats |
| `/home/dev/projects` | Your repositories |
| `/cache` | Installed agent versions and tools |

Host folders work in place of volumes. At every start, an empty folder mounted at one of these paths and owned by root is given to uid 1000, so the folder Docker creates for a missing bind mount path is ready to use. A folder with files in it must already be writable by uid 1000. Under rootless Docker or Podman, the new owner shows on the host as one of your subordinate uids.

> [!IMPORTANT]
> On an Ubuntu 24.04 host, Codex's sandbox also needs Ubuntu's AppArmor profile for bubblewrap. Ubuntu 26.04 loads it by default.
>
> ```sh
> sudo apt install apparmor-profiles
> sudo install -m 0644 /usr/share/apparmor/extra-profiles/bwrap-userns-restrict /etc/apparmor.d/bwrap-userns-restrict
> sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
> ```

## Set it up

1. Open `https://<host>:8443`, accept the self-signed certificate and set a password.
2. Install Claude Code or Codex under Agents and sign in.
3. Sign in to GitHub under Settings so agents can clone and push.

## Use it

Claude Code needs a Claude subscription sign-in, not an API key. It serves `~/projects` and each folder switched on under Folders, and the Claude app lists them under Remote Control.

For Codex, sign in with ChatGPT and pair your phone from the ⋯ menu next to Codex.

Clone repositories from the Folders card, or ask an agent started in `~/projects` to do it.

## Environment

| Variable | Default | Does |
|---|---|---|
| `EZRA_PORT` | `8443` | The port the manager listens on inside the container |
| `EZRA_SUDO` | `off` | `full` lets agents use sudo |
| `EZRA_APT_PACKAGES` | | Packages to install at every start, separated by spaces or commas |
| `EZRA_CHOWN_EMPTY_MOUNTS` | `on` | `off` leaves empty root-owned mounts of `/config`, `/home/dev/projects` and `/cache` owned by root |
| `EZRA_TLS_VERIFY` | `on` | `off` skips certificate checks on agent downloads, for networks that intercept TLS |
| `GH_TOKEN` or `GITHUB_TOKEN` | | A GitHub token to use in place of signing in |

Executable scripts mounted in `/etc/ezra/setup.d` run as root at every start, in name order.

## Add your own tools

For more than `EZRA_APT_PACKAGES` and `setup.d` scripts cover, build on this image:

```dockerfile
FROM tastypackets/ezra:latest

RUN apt-get update \
    && apt-get install --yes --no-install-recommends postgresql-client \
    && rm --recursive --force /var/lib/apt/lists/*

COPY setup.d/ /etc/ezra/setup.d/
```

Build steps run as root and then the container switches to the `dev` user that agents run as.

## Let agents use Docker

> [!WARNING]
> Access to the Docker socket is root access to the host. An agent with it can start a privileged container and read or change anything on the machine, including when a prompt injection in code it reads tells it to. Only add it on a machine you are willing to hand to the agents.

Add the host's socket, its group and the Docker CLI to the Compose service:

```yaml
services:
  ezra:
    group_add:
      - "<docker-group-id>" # the number from `stat -c %g /var/run/docker.sock` on the host
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
    environment:
      EZRA_APT_PACKAGES: docker.io docker-compose-v2
```

Containers the agents start run on the host, so their bind mounts use host paths, not paths inside ezra.

## Development

```sh
docker compose -f compose.dev.yaml up -d --build
```

`mise tasks` lists the build, test and code generation tasks.

## License

Apache-2.0
