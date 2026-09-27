import type { CodexProblem, CodexRemoteStatus, RelayState, ServerState } from "@ezra/client";
import { describe, expect, expectTypeOf, it } from "vitest";

import { CODEX_PROBLEM_LABELS } from "@/content/remote-control";

import { codexPairable, codexRemoteView } from "./codex-remote";
import type { CodexFix, RemoteView } from "./codex-remote";
import { formatDateTime } from "./utils";

const INSTALLED = "0.158.0";
const STATES = [
  "off",
  "waiting",
  "starting",
  "running",
  "retrying",
  "stopping",
] as const satisfies readonly ServerState[];
const RELAYS = [
  "disabled",
  "connecting",
  "connected",
  "errored",
] as const satisfies readonly RelayState[];
const PROBLEMS = [
  "mfa_required",
  "not_chatgpt",
  "signed_out",
  "not_allowed",
  "socket_in_use",
  "relay_unavailable",
  "unsupported_version",
] as const satisfies readonly CodexProblem[];
const USAGE = { chats: 0, running_chats: 0, memory_bytes: 150_000_000 };
const UPDATE = { version: "0.158.1", restart_by: "2026-09-27T18:00:00Z" };
const UPDATE_NOTE = {
  text: `Restarts on Codex 0.158.1 by ${formatDateTime(UPDATE.restart_by)}.`,
  tone: "muted",
};
const MFA_NOTE = {
  text: "Turn on multi-factor authentication in ChatGPT, then try again.",
  tone: "destructive",
};

function status(fields: Partial<CodexRemoteStatus> = {}): CodexRemoteStatus {
  return { state: "running", restarts: 0, ...fields };
}

function view(fields: Partial<CodexRemoteStatus> = {}): RemoteView {
  return codexRemoteView(status(fields), INSTALLED);
}

describe("codexRemoteView", () => {
  it("lists every state, relay and problem", () => {
    expectTypeOf<(typeof STATES)[number]>().toEqualTypeOf<ServerState>();
    expectTypeOf<(typeof RELAYS)[number]>().toEqualTypeOf<RelayState>();
    expectTypeOf<(typeof PROBLEMS)[number]>().toEqualTypeOf<CodexProblem>();
    expect(PROBLEMS.toSorted()).toEqual(Object.keys(CODEX_PROBLEM_LABELS).toSorted());
  });

  it("shows a dash while Codex is not installed", () => {
    expect(codexRemoteView(status({ problem: "mfa_required" }), undefined)).toEqual({
      label: "—",
    });
    expect(codexRemoteView(status({ state: "waiting" }), null)).toEqual({ label: "—" });
  });

  it.each([
    ["off", "Off"],
    ["waiting", "Waiting"],
    ["starting", "Starting"],
    ["retrying", "Restarting"],
    ["stopping", "Stopping"],
    ["running", "Running"],
  ] as const)("names the %s state with no note", (state, label) => {
    expect(view({ state })).toEqual({ label });
  });

  it.each([
    ["connecting", "Connecting"],
    ["disabled", "Paused"],
  ] as const)("names a %s relay with no note", (relay, label) => {
    expect(view({ relay, usage: USAGE })).toEqual({ label });
  });

  it("says Codex keeps trying after a failed connection", () => {
    expect(view({ relay: "errored", usage: USAGE })).toEqual({
      label: "Could not connect",
      note: { text: "Codex keeps trying to reach ChatGPT.", tone: "destructive" },
    });
  });

  it("counts chats only above zero, with the memory as the note", () => {
    const memory = { text: "Uses 150 MB", tone: "muted" };
    expect(view({ relay: "connected", usage: USAGE })).toEqual({
      label: "Connected",
      note: memory,
    });
    expect(view({ relay: "connected", usage: { ...USAGE, chats: 1 } })).toEqual({
      label: "Connected, 1 chat",
      note: memory,
    });
    expect(view({ relay: "connected", usage: { ...USAGE, chats: 3 } })).toEqual({
      label: "Connected, 3 chats",
      note: memory,
    });
    expect(view({ relay: "connected" })).toEqual({ label: "Connected" });
  });

  it("shows a waiting update over the memory", () => {
    expect(view({ relay: "connected", usage: { ...USAGE, chats: 2 }, update: UPDATE })).toEqual({
      label: "Connected, 2 chats",
      note: UPDATE_NOTE,
    });
    expect(view({ relay: "connecting", update: UPDATE })).toEqual({
      label: "Connecting",
      note: UPDATE_NOTE,
    });
  });

  it("keeps a problem's note over a waiting update, and shows the update under a problem without one", () => {
    expect(view({ relay: "disabled", problem: "mfa_required", update: UPDATE })).toEqual({
      label: "Needs MFA",
      note: MFA_NOTE,
      fix: "try_again",
    });
    expect(
      view({ relay: "disabled", problem: "signed_out", usage: USAGE, update: UPDATE }),
    ).toEqual({ label: "Needs a new sign-in", note: UPDATE_NOTE, fix: "sign_in" });
  });

  it.each([
    [
      "mfa_required",
      "Needs MFA",
      "Turn on multi-factor authentication in ChatGPT, then try again.",
      "try_again",
    ],
    ["not_chatgpt", "Needs a ChatGPT sign-in", undefined, "sign_in_with_chatgpt"],
    ["signed_out", "Needs a new sign-in", undefined, "sign_in"],
    [
      "not_allowed",
      "Not allowed",
      "Codex's managed requirements turn remote control off.",
      undefined,
    ],
    ["socket_in_use", "Blocked", "Another Codex server was running in this box.", undefined],
    ["relay_unavailable", "Could not connect", "Codex keeps trying to reach ChatGPT.", undefined],
  ] satisfies [CodexProblem, string, string | undefined, CodexFix | undefined][])(
    "shows %s over the state and relay",
    (problem, label, text, fix) => {
      for (const state of STATES) {
        for (const relay of [...RELAYS, undefined]) {
          const shown = view({ state, relay, problem, usage: USAGE });
          expect(shown.label).toBe(label);
          expect(shown.note).toEqual(text ? { text, tone: "destructive" } : undefined);
          const answers = state === "running" && relay !== undefined;
          expect(shown.fix).toBe(fix === "try_again" && !answers ? undefined : fix);
        }
      }
    },
  );

  it("offers Try again only while a running server answers", () => {
    expect(view({ relay: "disabled", problem: "mfa_required" })).toEqual({
      label: "Needs MFA",
      note: MFA_NOTE,
      fix: "try_again",
    });
    expect(view({ problem: "mfa_required" }).fix).toBeUndefined();
    expect(view({ state: "retrying", problem: "mfa_required" }).fix).toBeUndefined();
    expect(view({ state: "waiting", problem: "mfa_required" }).fix).toBeUndefined();
    expect(view({ state: "retrying", problem: "signed_out" }).fix).toBe("sign_in");
  });

  it("names the installed version Codex cannot run", () => {
    expect(
      codexRemoteView(status({ state: "waiting", problem: "unsupported_version" }), "0.158.0"),
    ).toEqual({
      label: "Not supported",
      note: { text: "Codex 0.158.0 cannot run remote control here.", tone: "destructive" },
    });
  });

  it("names the older version that keeps running when the installed one cannot", () => {
    expect(
      view({
        relay: "connected",
        problem: "unsupported_version",
        server_version: "0.157.1",
        usage: USAGE,
      }),
    ).toEqual({
      label: "Not supported",
      note: {
        text: "Codex 0.158.0 cannot run remote control here, so 0.157.1 keeps running.",
        tone: "destructive",
      },
    });
  });

  it("never repeats the label in the note", () => {
    for (const state of STATES) {
      for (const relay of [...RELAYS, undefined]) {
        for (const problem of [...PROBLEMS, undefined]) {
          for (const usage of [USAGE, { ...USAGE, chats: 1 }, undefined]) {
            for (const update of [UPDATE, undefined]) {
              const shown = view({
                state,
                relay,
                problem,
                usage,
                update,
                server_version: "0.157.1",
              });
              expect(shown.label).not.toBe("");
              expect(shown.note?.text).not.toBe(shown.label);
            }
          }
        }
      }
    }
  });
});

describe("codexPairable", () => {
  it("pairs only while Codex runs connected to ChatGPT", () => {
    for (const state of STATES) {
      for (const relay of [...RELAYS, undefined]) {
        for (const problem of [...PROBLEMS, undefined]) {
          expect(codexPairable(status({ state, relay, problem }))).toBe(
            state === "running" && relay === "connected",
          );
        }
      }
    }
  });
});
