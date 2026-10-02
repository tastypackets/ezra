import { describe, expect, it } from "vitest";

import {
  effortSuggestions,
  inboundDraft,
  modelSuggestions,
  settingsFromDraft,
} from "./inbound-settings";

describe("Codex shortcut suggestions", () => {
  const models = [
    {
      model: "first",
      display_name: "First",
      supported_reasoning_efforts: [
        { reasoning_effort: "low" },
        { reasoning_effort: "high", description: "More reasoning" },
      ],
    },
    {
      model: "second",
      description: "Second model",
      supported_reasoning_efforts: [
        { reasoning_effort: "low" },
        { reasoning_effort: "future-effort" },
      ],
    },
  ];
  it("suggests model identifiers and only the selected model's efforts", () => {
    expect(modelSuggestions(models)).toEqual([
      { value: "first", description: "First" },
      { value: "second", description: "Second model" },
    ]);
    expect(effortSuggestions(models, " first ").map((option) => option.value)).toEqual([
      "low",
      "high",
    ]);
    expect(effortSuggestions(models, "second").map((option) => option.value)).toEqual([
      "low",
      "future-effort",
    ]);
  });
  it("combines reported efforts for an unknown or unchanged model and supports unavailable catalogs", () => {
    for (const model of ["", "custom-model"]) {
      expect(effortSuggestions(models, model).map((option) => option.value)).toEqual([
        "low",
        "high",
        "future-effort",
      ]);
    }
    expect(effortSuggestions([{ model: "old-model" }], "old-model")).toEqual([]);
    expect(modelSuggestions([])).toEqual([]);
  });
});

describe("inbound settings drafts", () => {
  it("defaults to reactions and keeps footer editing explicitly selected", () => {
    expect(inboundDraft({}).feedback).toBe("reactions");
    for (const feedback of ["off", "reactions", "footer"]) {
      const draft = { ...inboundDraft({}), feedback };
      const saved = settingsFromDraft({}, draft);
      expect(saved.github.edit_comment_status).toBe(feedback === "footer");
      expect(saved.github.react_on_status).toBe(feedback === "reactions");
      expect(inboundDraft(saved).feedback).toBe(feedback);
    }
  });
  it("defaults polling to thirty seconds and validates positive whole seconds", () => {
    const draft = inboundDraft({});
    expect(draft.poll_interval_seconds).toBe("30");
    for (const interval of ["", "0", "-1", "1.5", "NaN", "4294967296"]) {
      draft.poll_interval_seconds = interval;
      expect(() => settingsFromDraft({}, draft)).toThrow("Polling interval");
    }
    draft.poll_interval_seconds = "10";
    expect(settingsFromDraft({}, draft).github.poll_interval_seconds).toBe(10);
    expect(inboundDraft(settingsFromDraft({}, draft)).poll_interval_seconds).toBe("10");
  });
  it("defaults repository scope on and persists either choice", () => {
    const draft = inboundDraft({});
    expect(draft.only_added_repositories).toBe(true);
    draft.only_added_repositories = false;
    const saved = settingsFromDraft({}, draft);
    expect(saved.github.only_added_repositories).toBe(false);
    expect(inboundDraft(saved).only_added_repositories).toBe(false);
  });
  it("defaults waiting expiry to one day and validates positive whole hours", () => {
    const draft = inboundDraft({});
    expect(draft.waiting_expiry_hours).toBe("24");
    for (const expiry of ["", "0", "-1", "1.5", "NaN", "4294967296"]) {
      draft.waiting_expiry_hours = expiry;
      expect(() => settingsFromDraft({}, draft)).toThrow("Waiting expiry");
    }
    draft.waiting_expiry_hours = "48";
    const saved = settingsFromDraft({}, draft);
    expect(saved.waiting_expiry_hours).toBe(48);
    expect(inboundDraft(saved).waiting_expiry_hours).toBe("48");
  });
  it("preserves limits and removes empty model overrides", () => {
    const settings = {
      max_queued_events: 12,
      github: { max_concurrent_requests: 2, only_added_repositories: false },
    };
    const draft = inboundDraft(settings);
    draft.shortcuts = [{ trigger: "/custom", model: "", effort: " high " }];
    const saved = settingsFromDraft(settings, draft);
    expect(saved.max_queued_events).toBe(12);
    expect(saved.github.max_concurrent_requests).toBe(2);
    expect(saved.github.only_added_repositories).toBe(false);
    expect(saved.shortcuts).toEqual({ "/custom": { agent: "codex", effort: "high" } });
  });

  it("rejects duplicate commands and invalid retention before saving", () => {
    const draft = inboundDraft({});
    draft.shortcuts.push({ trigger: " /ezra ", model: "", effort: "" });
    expect(() => settingsFromDraft({}, draft)).toThrow();
    draft.shortcuts = [];
    draft.retention_days = "NaN";
    expect(() => settingsFromDraft({}, draft)).toThrow();
    draft.retention_days = "";
    expect(() => settingsFromDraft({}, draft)).toThrow();
    draft.retention_days = "0";
    expect(settingsFromDraft({}, draft).retention_days).toBe(0);
    expect(settingsFromDraft({}, draft).shortcuts).toEqual({});
  });
});
