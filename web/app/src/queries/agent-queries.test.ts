import type { AgentStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { INSTALL_POLL_MS, isClaudeInstalled, pollInterval } from "./agent-queries";

const idle: AgentStatus = { agent: "claude", configured: false, logged_in: false };

describe("pollInterval", () => {
  it("does not poll while nothing is in progress", () => {
    expect(pollInterval(undefined)).toBe(false);
    expect(pollInterval([idle, { ...idle, agent: "codex" }])).toBe(false);
  });

  it("polls fast while a download runs", () => {
    const installing = { ...idle, install_progress: { received_bytes: 1, total_bytes: 10 } };
    expect(pollInterval([installing])).toBe(INSTALL_POLL_MS);
  });

  it("does not poll while a sign-in waits", () => {
    const codex = { url: "https://auth.openai.com/codex/device", code: "ABCD-1234" };
    const claude = { url: "https://claude.com/oauth" };
    expect(
      pollInterval([
        { ...idle, login_prompt: claude },
        { ...idle, agent: "codex", login_prompt: codex },
      ]),
    ).toBe(false);
  });
});

describe("isClaudeInstalled", () => {
  it("follows Claude Code's installed version, not Codex's", () => {
    const codex: AgentStatus = { ...idle, agent: "codex", installed_version: "0.1.0" };
    expect(isClaudeInstalled([idle, codex])).toBe(false);
    expect(isClaudeInstalled([{ ...idle, installed_version: "2.1.283" }, codex])).toBe(true);
  });
});
