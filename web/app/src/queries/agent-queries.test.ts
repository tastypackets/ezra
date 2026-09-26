import type { AgentStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { INSTALL_POLL_MS, pollInterval } from "./agent-queries";

const idle: AgentStatus = { agent: "claude", configured: false, logged_in: false };

function server(state: "starting" | "running" | "retrying"): AgentStatus {
  return { ...idle, remote_control: { state, restarts: 0 } };
}

describe("pollInterval", () => {
  it("does not poll while nothing is in progress", () => {
    expect(pollInterval(undefined)).toBe(false);
    expect(pollInterval([idle, { ...idle, agent: "codex" }])).toBe(false);
  });

  it("polls fast while a download runs", () => {
    const installing = { ...idle, install_progress: { received_bytes: 1, total_bytes: 10 } };
    expect(pollInterval([installing])).toBe(INSTALL_POLL_MS);
  });

  it("polls while Codex waits on the website", () => {
    const prompt = { url: "https://auth.openai.com/codex/device", code: "ABCD-1234" };
    expect(pollInterval([{ ...idle, agent: "codex", login_prompt: prompt }])).toBe(3_000);
  });

  it("polls while Remote Control starts, slowly while it restarts, not while it runs", () => {
    expect(pollInterval([server("starting")])).toBe(3_000);
    expect(pollInterval([server("retrying")])).toBe(15_000);
    expect(pollInterval([server("running")])).toBe(false);
  });

  it("does not poll while Claude waits for a pasted code", () => {
    const prompt = { url: "https://claude.com/oauth" };
    expect(pollInterval([{ ...idle, login_prompt: prompt }])).toBe(false);
  });
});
