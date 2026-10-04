import type { Agent, AgentModel, AgentStatus, InboundSettings, Shortcut } from "@ezra/client";

import { INBOUND_COPY } from "@/content/inbound";

/** The models `agent` listed when it last started, empty until it has. */
export function agentModels(agents: readonly AgentStatus[] | undefined, agent: Agent) {
  return agents?.find((status) => status.agent === agent)?.models ?? [];
}

export function modelSuggestions(models: readonly AgentModel[]) {
  return models.map((model) => ({
    value: model.model,
    description: model.description || model.display_name || undefined,
  }));
}

/** The selected model's efforts, or every listed effort for a model the agent did not list. */
export function effortSuggestions(models: readonly AgentModel[], selectedModel: string) {
  const selected = models.find((model) => model.model === selectedModel.trim());
  const candidates = selected ? [selected] : models;
  return [
    ...new Map(
      candidates.flatMap((model) =>
        model.efforts.map(
          (effort) =>
            [
              effort.effort,
              { value: effort.effort, description: effort.description || undefined },
            ] as const,
        ),
      ),
    ).values(),
  ];
}

export function inboundDraft(settings: InboundSettings) {
  return {
    shortcuts: Object.entries<Shortcut>(settings.shortcuts ?? { "/ezra": { agent: "codex" } }).map(
      ([trigger, shortcut]) => ({
        trigger,
        agent: shortcut.agent,
        model: shortcut.model ?? "",
        effort: shortcut.effort ?? "",
      }),
    ),
    retention_days: String(settings.retention_days ?? 90),
    waiting_expiry_hours: String(settings.waiting_expiry_hours ?? 24),
    poll_interval_seconds: String(settings.github?.poll_interval_seconds ?? 30),
    only_added_repositories: settings.github?.only_added_repositories ?? true,
    feedback: settings.github?.edit_comment_status
      ? "footer"
      : settings.github?.react_on_status === false
        ? "off"
        : "reactions",
  };
}

export type ShortcutDraft = ReturnType<typeof inboundDraft>["shortcuts"][number];

export function withAgent(shortcut: ShortcutDraft, agent: Agent): ShortcutDraft {
  return shortcut.agent === agent ? shortcut : { ...shortcut, agent, model: "", effort: "" };
}

export function settingsFromDraft(
  settings: InboundSettings,
  draft: ReturnType<typeof inboundDraft>,
) {
  const commands = draft.shortcuts.map((shortcut) => shortcut.trigger.trim());
  if (commands.some((command) => !command)) throw new Error(INBOUND_COPY.commandRequired);
  if (new Set(commands).size !== commands.length) throw new Error(INBOUND_COPY.duplicate);
  const retentionDays = Number(draft.retention_days);
  const waitingExpiryHours = Number(draft.waiting_expiry_hours);
  if (
    !draft.waiting_expiry_hours.trim() ||
    !Number.isInteger(waitingExpiryHours) ||
    waitingExpiryHours < 1 ||
    waitingExpiryHours > 4_294_967_295
  ) {
    throw new Error(INBOUND_COPY.invalidWaitingExpiry);
  }
  const pollIntervalSeconds = Number(draft.poll_interval_seconds);
  if (
    !draft.poll_interval_seconds.trim() ||
    !Number.isInteger(pollIntervalSeconds) ||
    pollIntervalSeconds < 1 ||
    pollIntervalSeconds > 4_294_967_295
  ) {
    throw new Error(INBOUND_COPY.invalidPollInterval);
  }
  if (
    !draft.retention_days.trim() ||
    !Number.isInteger(retentionDays) ||
    retentionDays < 0 ||
    retentionDays > 4_294_967_295
  ) {
    throw new Error(INBOUND_COPY.invalidRetention);
  }
  return {
    ...settings,
    retention_days: retentionDays,
    waiting_expiry_hours: waitingExpiryHours,
    github: {
      ...settings.github,
      poll_interval_seconds: pollIntervalSeconds,
      only_added_repositories: draft.only_added_repositories,
      edit_comment_status: draft.feedback === "footer",
      react_on_status: draft.feedback === "reactions",
    },
    shortcuts: Object.fromEntries(
      draft.shortcuts.map((shortcut) => [
        shortcut.trigger.trim(),
        {
          agent: shortcut.agent,
          ...(shortcut.model.trim() ? { model: shortcut.model.trim() } : {}),
          ...(shortcut.effort.trim() ? { effort: shortcut.effort.trim() } : {}),
        },
      ]),
    ),
  };
}
