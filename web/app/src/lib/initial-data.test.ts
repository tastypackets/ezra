import { describe, expect, it } from "vitest";

import { isInitialData } from "./initial-data";

describe("isInitialData", () => {
  it("accepts what the manager writes", () => {
    const session = { claimed: true, authenticated: false };
    expect(
      isInitialData({
        revision: 1,
        session,
        agents: null,
        folders: null,
        clones: null,
        remote_control: null,
        git: null,
      }),
    ).toBe(true);
    const remoteControl = { projects: { state: "waiting", restarts: 0 }, folders: {} };
    expect(
      isInitialData({
        revision: 1,
        session,
        agents: [],
        folders: [],
        clones: [],
        remote_control: remoteControl,
        git: { github: { signed_in: false, from_environment: false }, identity: null },
      }),
    ).toBe(true);
    expect(
      isInitialData({
        revision: 1,
        session,
        agents: [],
        folders: [],
        clones: [],
        remote_control: remoteControl,
      }),
    ).toBe(false);
  });

  it("rejects anything else", () => {
    expect(isInitialData(null)).toBe(false);
    expect(isInitialData({ session: null, agents: null })).toBe(false);
    expect(isInitialData({ session: { claimed: true }, agents: null })).toBe(false);
    expect(isInitialData({ session: { claimed: true, authenticated: true } })).toBe(false);
    expect(
      isInitialData({ session: { claimed: true, authenticated: true }, agents: [], folders: [] }),
    ).toBe(false);
    expect(isInitialData({ session: { claimed: true, authenticated: true }, agents: {} })).toBe(
      false,
    );
  });
});
