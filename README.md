# ezra

A container that runs Claude Code and Codex for the Claude and ChatGPT apps, managed from a web page on port 8443.

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
      - home:/home/dev
      - cache:/cache
    stop_grace_period: 60s
    # Codex's sandbox needs these for the ChatGPT app's folder picker.
    # Remove them if you only use Claude Code.
    security_opt:
      - seccomp=unconfined
      - apparmor=unconfined

volumes:
  config:
  home:
  cache:
```

| Volume | Holds |
|---|---|
| `/config` | The manager password, GitHub settings and mise |
| `/home/dev` | Repositories in `projects`, agent data in `.claude` and `.codex`, and other user files |
| `/cache` | Installed agent versions and tools |

At start, an empty folder mounted at one of these paths and owned by root is given to uid 1000.

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

Clone repositories from the Folders card, or ask an agent started in `~/projects` to do it.

### Pair the ChatGPT app

Sign in to Codex with ChatGPT on ezra web, then open the ⋯ menu next to Codex and choose Pair ChatGPT app. Leave the pairing dialog open while you connect from desktop app or mobile phone.

On Desktop:

1. Open the ChatGPT app and go to Settings → Pair → Control other devices.
2. Click Add and enter the code shown in ezra's pairing dialog.

On mobile:

1. Open the ChatGPT app's menu and choose Remote.
2. Open Settings or Add remote, then choose Add connection → Pair new device.
3. Scan the QR code shown in ezra's pairing dialog.

### Add a project

After pairing, in the ChatGPT app:

1. Click + → New project → Add folder.
2. Change This computer to the hostname of your ezra instance.
3. Click Add, open `projects`, and select your repository.

### Trigger an agent from GitHub

1. Sign in to GitHub under Settings, and install and sign in to the agent your shortcut uses under Agents. For Codex, start it under Agents. For Claude Code, keep Serve to the Claude app on under Settings.
2. Clone the repository into `~/projects`. Its `origin` must match the configured GitHub host and repository.
3. Under GitHub triggers, turn off Only repositories added to Ezra to accept commands from other repositories. New Codex chats there start in `/home/dev`, and new Claude Code sessions in `/home/dev/projects`.
4. Post `/ezra fix this` in an issue or pull request conversation comment using your signed-in GitHub account. A bare `/ezra` asks the agent to act on that issue or pull request.

Ezra polls your issue and pull request conversation comments through GitHub GraphQL using the existing `gh` sign-in, then filters to repositories added to Ezra unless that setting is off. The polling interval defaults to 30 seconds after the previous scan finishes and can be changed under GitHub triggers. No GitHub App, webhook endpoint, or CI runner is required. Polling starts from the first scan after activation. Comments from other accounts, issue descriptions, and inline review comments do not trigger sessions. Shortcuts match literal text, including text inside quotes and code. Edits do not rerun an already queued comment.

Successful scans save the timestamp from GitHub's first response, and the next scan starts five seconds earlier with comment IDs preventing duplicate delivery. Failed scans keep the previous timestamp.

Each discussion keeps one chat per agent. A request goes to the discussion's chat for its shortcut's agent, so Codex and Claude Code requests on the same discussion reach separate chats that later requests of each agent reuse. Requests in a discussion wait for older requests of the same agent, not for the other agent's requests.

A request waits while Ezra has its agent's server stopped or starting on purpose, such as while Ezra starts, after a settings change, an update or a sign-in through Ezra, or while the server is turned off. Otherwise a request that cannot reach its agent fails right away, such as when the server stopped unexpectedly or the agent is not installed or not signed in. A request that waits longer than the waiting request expiry fails.

When a discussion has no chat for the requested agent, Ezra reads GitHub's direct issue and pull request links, including closed and merged pull requests. Multiple links can reuse one distinct mapped chat of the requested agent on the same Ezra host, including chats in another checkout. Ezra starts a separate chat when no linked discussion has a mapping for that agent or the mappings point to different chats. Failed lookups and unfinished linked routing stay pending. Existing discussion mappings keep their chat when GitHub links change.

Add `--new` immediately after the command, such as `/ezra --new fix this`, to start a fresh chat of the shortcut's agent for that discussion. Older requests for that agent are delivered first. The discussion's chat for the other agent stays mapped, and other discussions sharing the old chat keep their route. A saved request still runs if its comment is edited or deleted. Edits do not change its original routing instruction.

A new chat's requested name includes the repository, discussion number and title. A new Codex chat uses the repository's existing checkout, or `/home/dev` when no unique checkout is available, and the nearest matching Codex project when it is unique, otherwise no project association. Where a new Claude Code session works is described under Claude Code requests. Ezra itself does not create a worktree or check out the pull request branch. Requests keep the comment text, discussion URL and title. The description is included only in the first message to a new chat, including a fresh or replacement chat. Follow-up requests keep the chat's current name. Open the ChatGPT or Claude app to follow progress or steer the session.

Settings can map custom commands such as `/ezra-fast` to an agent, a model and an effort. The default `/ezra` and shortcuts saved without an agent use Codex. For Codex, model and effort change the chat defaults for subsequent turns, including queued work. The inputs suggest the models each agent listed when it last started, and the efforts a chosen model takes. Codex lists them when Ezra connects to it, and Claude Code when its `~/projects` server starts. Inputs also accept manual values. Blank fields keep the current defaults. A comment containing different configured shortcuts is rejected as ambiguous.

Status feedback defaults to reactions, with a rocket for confirmed delivery and a confused face when attention is needed. Under GitHub triggers, choose a status footer or turn feedback off. Footers edit your original comment with received, delivered, unconfirmed, or failed status and the assigned chat name when available. Each update reads the current comment before writing and has a five-second overall timeout. Feedback uses your GitHub identity and never posts a new comment. It is best effort and each status is attempted once. Concurrent edits or a timed-out write can leave stale status. Database delivery records remain authoritative, so feedback failures and comment edits do not repeat agent delivery. Delivered means the session accepted the message, not that the work finished. Agent answers remain in the app unless you ask the agent to reply on GitHub.

Archived Codex chats are restored automatically when possible. A definitively missing chat is replaced and its mapping updated. Timeouts and unknown delivery results stay uncertain to avoid sending the same request twice. Check the app and manager logs before posting a new request in that case.

#### Claude Code requests

A shortcut set to Claude Code creates a Claude Code session in the Remote Control environment of the served folder that holds the repository's checkout, so the Claude app lists it with that folder's other sessions. A repository whose folder is not served, or that has no unique checkout, gets a session from the `/home/dev/projects` server. Each session counts against the server's Sessions per folder, and in worktree mode Remote Control gives it a worktree of its own.

Each Claude Code server pauses on its own, so while one restarts, requests for other served folders go through. A request also fails right away when its server cannot take sessions, such as a server whose Sessions per folder is 1 and so shows a session link instead of its environment.

Ezra queues each request in the session with `claude -p --cloud <session>`, and delivered means that command reported the message as queued. Model and effort apply when Ezra creates the session, and later requests keep the session's model and effort.

Before each request Ezra reads the session. A session that is archived, unknown to Claude, or in an environment no running server is connected in is replaced with a new session that gets the description again. Archived sessions are not restored. Remote Control environments change when the container is recreated, so the first request for an older session after that starts a new one.

Creating and reading sessions uses an unofficial Claude Code endpoint that may change with Claude Code releases.

Integration metadata lives in `/config/ezra/ezra.db`. Cleanup runs at startup and daily, with a configurable 90-day history default and count and byte limits. Queued and uncertain requests are kept within the queue limits. Cleanup does not delete native chats or repositories. After an idle mapping expires, a new trigger can start a new chat. Routing is local to this Ezra instance.

## Environment

| Variable | Default | Does |
|---|---|---|
| `EZRA_PORT` | `8443` | The port the manager listens on inside the container |
| `EZRA_SUDO` | `off` | `full` lets agents use sudo |
| `EZRA_APT_PACKAGES` | | Packages to install at every start, separated by spaces or commas |
| `EZRA_CHOWN_EMPTY_MOUNTS` | `on` | `off` leaves empty root-owned mounts of `/config`, `/home/dev`, `/home/dev/projects` and `/cache` owned by root |
| `EZRA_TLS_VERIFY` | `on` | `off` skips certificate checks on agent downloads, for networks that intercept TLS |
| `RUST_LOG` | `info` | Manager log filter, such as `warn` or `warn,ezra::inbound=debug`. Codex uses its own log filter |
| `GH_HOST` | `github.com` | The GitHub host to sign in to, such as a GitHub Enterprise Server host |
| `GH_TOKEN` or `GITHUB_TOKEN` | | A token for github.com or a `ghe.com` subdomain to use in place of signing in |
| `GH_ENTERPRISE_TOKEN` or `GITHUB_ENTERPRISE_TOKEN` | | A token for a GitHub Enterprise Server host to use in place of signing in |

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
