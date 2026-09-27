import type { RemoteControlOverview, RemoteControlStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { remoteControlSummary, serversRun } from "./remote-control";

const running: RemoteControlStatus = {
  state: "running",
  restarts: 0,
  usage: { sessions: 1, memory_bytes: 1 },
};

function overview(
  projects: RemoteControlStatus,
  folders: Record<string, RemoteControlStatus> = {},
): RemoteControlOverview {
  return { device: "box", projects, folders, codex: { state: "waiting", restarts: 0 } };
}

describe("remoteControlSummary", () => {
  it("says when Remote Control is off or waits", () => {
    expect(remoteControlSummary(overview({ state: "off", restarts: 0 }))).toBe("Off");
    expect(remoteControlSummary(overview({ state: "waiting", restarts: 0 }))).toBe("Waiting");
  });

  it("counts the servers and the sessions in them", () => {
    expect(
      remoteControlSummary(
        overview(running, {
          app: { ...running, usage: { sessions: 2, memory_bytes: 1 } },
          docs: { state: "starting", restarts: 0 },
        }),
      ),
    ).toBe("3 servers, 3 sessions");
    expect(remoteControlSummary(overview({ ...running, usage: null }))).toBe(
      "1 server, 0 sessions",
    );
  });

  it("leaves out folders that wait", () => {
    expect(
      remoteControlSummary(overview(running, { app: { state: "waiting", restarts: 0 } })),
    ).toBe("1 server, 1 session");
  });
});

describe("serversRun", () => {
  const waiting: RemoteControlStatus = { state: "waiting", restarts: 0 };

  it("counts only the agent's own running servers", () => {
    expect(serversRun("claude", overview(waiting))).toBe(false);
    expect(serversRun("claude", overview(waiting, { app: running }))).toBe(true);
    expect(serversRun("codex", overview(running))).toBe(false);
    expect(
      serversRun("codex", { ...overview(waiting), codex: { state: "running", restarts: 0 } }),
    ).toBe(true);
    expect(
      serversRun("claude", { ...overview(waiting), codex: { state: "running", restarts: 0 } }),
    ).toBe(false);
  });
});
