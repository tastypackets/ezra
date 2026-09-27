import type { CodexProblem, CodexRemoteStatus } from "@ezra/client";
import prettyBytes from "pretty-bytes";

import { AGENT_NAMES } from "@/content/agents";
import {
  CODEX_PROBLEM_LABELS,
  CODEX_PROBLEM_NOTES,
  CODEX_RELAY,
  REMOTE_CONTROL_DESCRIPTIONS,
  SERVER_STATES,
} from "@/content/remote-control";
import { formatDateTime } from "@/lib/utils";

/** The button in an agent's row that fixes what keeps Codex from the ChatGPT app. */
export type CodexFix = "try_again" | "sign_in_with_chatgpt" | "sign_in";

export interface RemoteNote {
  text: string;
  tone: "destructive" | "muted";
}

/** An agent's Remote cell: one line, at most one note under it, and the fix for a problem. */
export interface RemoteView {
  label: string;
  note?: RemoteNote;
  fix?: CodexFix;
}

const FIXES: Partial<Record<CodexProblem, CodexFix>> = {
  mfa_required: "try_again",
  not_chatgpt: "sign_in_with_chatgpt",
  signed_out: "sign_in",
};

/**
 * Codex's Remote cell, `—` while Codex is not installed. A problem wins over the state and relay.
 * Trying again needs a running server that answers.
 */
export function codexRemoteView(
  status: CodexRemoteStatus,
  installedVersion: string | null | undefined,
): RemoteView {
  if (!installedVersion) {
    return { label: "—" };
  }
  const note = problemNote(status, installedVersion) ?? runningNote(status);
  if (status.problem) {
    const fix = FIXES[status.problem];
    const answers = status.state === "running" && Boolean(status.relay);
    return {
      label: CODEX_PROBLEM_LABELS[status.problem],
      note,
      fix: fix === "try_again" && !answers ? undefined : fix,
    };
  }
  return { label: stateLabel(status), note };
}

function stateLabel({ state, relay, usage }: CodexRemoteStatus): string {
  if (state !== "running" || !relay) {
    return SERVER_STATES[state];
  }
  return relay === "connected"
    ? REMOTE_CONTROL_DESCRIPTIONS.codex_connected(usage?.chats ?? 0)
    : CODEX_RELAY[relay];
}

function problemNote(
  { problem, state, server_version: running }: CodexRemoteStatus,
  installedVersion: string,
): RemoteNote | undefined {
  if (!problem) {
    return undefined;
  }
  if (problem === "unsupported_version") {
    return {
      text:
        state === "running" && running
          ? REMOTE_CONTROL_DESCRIPTIONS.codex_unsupported_kept(installedVersion, running)
          : REMOTE_CONTROL_DESCRIPTIONS.codex_unsupported(installedVersion),
      tone: "destructive",
    };
  }
  const text = CODEX_PROBLEM_NOTES[problem];
  return text ? { text, tone: "destructive" } : undefined;
}

function runningNote({
  state,
  relay,
  problem,
  update,
  usage,
}: CodexRemoteStatus): RemoteNote | undefined {
  if (state !== "running") {
    return undefined;
  }
  if (!problem && relay === "errored") {
    return { text: REMOTE_CONTROL_DESCRIPTIONS.codex_relay_errored_note, tone: "destructive" };
  }
  if (update) {
    return {
      text: REMOTE_CONTROL_DESCRIPTIONS.update_waiting(
        AGENT_NAMES.codex,
        update.version,
        formatDateTime(update.restart_by),
      ),
      tone: "muted",
    };
  }
  if (!problem && relay === "connected" && usage) {
    return {
      text: REMOTE_CONTROL_DESCRIPTIONS.codex_memory(prettyBytes(usage.memory_bytes)),
      tone: "muted",
    };
  }
  return undefined;
}
