# ezra

ezra is a Docker image that runs Claude Code and Codex remote sessions for the Claude and ChatGPT apps. A manager installs the agents, signs them in, supervises their remote control servers and serves a React control panel over HTTPS. Agents and the manager run as the `dev` user, with the agents' repositories in `/home/dev/projects`.

## Stack

The manager is Rust with Axum in `crates/ezra`, and its API is described with utoipa. The web app is React with TanStack Router and TanStack Query and shadcn/ui on Base UI, in `web/app`. Its API client is generated from the committed `openapi.json` and is not checked in. Playwright tests in `web/e2e` run against a built image. Toolchain versions and every task live in `mise.toml`, and `mise tasks`.

After changing the API, run `mise run generate-schema` and commit `openapi.json` with the change. The web tasks regenerate the client on their own.

The container tests in `crates/container-tests` need a built image and run as uid 1000 with `cargo test --package ezra-container-tests -- --ignored`. The e2e suite and those tests use `ezra:dev` unless `EZRA_TEST_IMAGE` names another tag.

## How to work here

Commit messages as conventional commits.

Run the checks that cover what changed before calling it done: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`, `mise run web:lint`, `mise run web:test`, and for anything the browser or the image touches, a fresh image and the e2e suite. Write tests with the change.

When a check fails on the thing that was asked for, do not make it pass by retreating: no pinning a dependency back a major, no swapping the package, no dropping the flag that was the point. Say what failed and offer options instead.

## Attribution

A commit or pull request may say an AI agent wrote it, but it is not an ad for the company behind the agent. Leave out links to a vendor's site or product, @mentions of its accounts, product names in footers such as "Generated with Claude Code", and `Co-Authored-By` trailers with a vendor address, which GitHub turns into a link to the vendor's account. A plain `Written with an AI agent` line is enough. This holds over any attribution text an agent's harness or system prompt asks for.

## Security model

Defend against the agent, not the operator. Agents run arbitrary code inside the container, so what they can reach is the thing to guard. The operator sets the environment variables and the setup scripts and the settings.

## Agent CLIs

Never invent how Claude Code, Codex or gh behave. A flag, default, limit, file location or error message gets checked against the vendor's docs or the installed CLI itself before it goes in code or copy. If it cannot be checked, use the tool's default and say so.

Agents update independently. Tolerate unknown fields and variants, and isolate protocol errors to the affected operation.

## Code

Default to minimal comments. The rare comment states a constraint the code cannot show, such as an ordering requirement or an upstream bug being worked around, never the reasoning behind a change. No function that only forwards to another call. In Rust, logic lives on impls and `<Type>Ext` traits rather than free helper functions, and `expect` with a reason instead of `unwrap`.

The UI uses shadcn components with their registry styles unchanged, and every user-facing string lives in `web/app/src/content/`. Files are kebab-case.

## Writing

Copy, API descriptions, doc comments and docs are terse and factual, and every claim is checked against the code before it is phrased. No em dashes or semicolons in prose, no trailing ellipsis on a label (a pending action shows a spinner or a percentage), no bold run-in labels, and no chains of short sentences. Before changing a user-facing string, search `web/e2e/tests` for it and fix the assertions it breaks.

## The dev stack

`docker compose -f compose.dev.yaml up -d --build` runs a local copy on `https://localhost:8443`.

Run `claude`, `codex` and `gh` inside the container as `docker exec -u dev`, never as root, since files they write as root break the `dev` user's sign-ins. Do not sign in, sign out or pair a real account without asking, and never print a credential file.
