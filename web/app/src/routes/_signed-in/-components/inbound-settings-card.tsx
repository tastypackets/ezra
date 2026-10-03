import type { Agent } from "@ezra/client";
import {
  getInboundSettingsOptions,
  updateInboundSettingsMutation,
} from "@ezra/client/react-query.gen";
import { useForm, useSelector } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useId, useState } from "react";

import { Button } from "@/components/ui/button";
import { Autocomplete } from "@/components/ui/autocomplete";
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Field,
  FieldDescription,
  FieldContent,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/toast";
import { AGENT_NAMES } from "@/content/agents";
import { INBOUND_COPY } from "@/content/inbound";
import {
  agentModels,
  effortSuggestions,
  inboundDraft,
  modelSuggestions,
  settingsFromDraft,
  withAgent,
} from "@/lib/inbound-settings";
import { agentsQueryOptions } from "@/queries/agent-queries";
import { errorMessage } from "@/lib/utils";

const SHORTCUT_AGENTS: readonly Agent[] = ["claude", "codex"];

export function InboundSettingsCard() {
  const identifier = useId();
  const queryClient = useQueryClient();
  const { data: settings } = useSuspenseQuery(getInboundSettingsOptions());
  const [error, setError] = useState<string>();
  const save = useMutation({
    ...updateInboundSettingsMutation(),
    onSuccess: (saved) => queryClient.setQueryData(getInboundSettingsOptions().queryKey, saved),
  });
  const form = useForm({
    defaultValues: inboundDraft(settings),
    onSubmit: async ({ value, formApi }) => {
      setError(undefined);
      try {
        const saved = await save.mutateAsync({ body: settingsFromDraft(settings, value) });
        formApi.reset(inboundDraft(saved));
        toast.add({ title: INBOUND_COPY.saved });
      } catch (failure) {
        setError(errorMessage(failure));
      }
    },
  });
  const agents = useQuery(agentsQueryOptions);
  const usesClaude = useSelector(form.store, (state) =>
    state.values.shortcuts.some((shortcut) => shortcut.agent === "claude"),
  );
  const usesCodex = useSelector(form.store, (state) =>
    state.values.shortcuts.some((shortcut) => shortcut.agent === "codex"),
  );
  const unlisted = SHORTCUT_AGENTS.filter(
    (agent) =>
      (agent === "claude" ? usesClaude : usesCodex) && agentModels(agents.data, agent).length === 0,
  );
  return (
    <Card id="inbound" aria-labelledby={identifier}>
      <CardHeader>
        <CardTitle id={identifier}>{INBOUND_COPY.title}</CardTitle>
        <FieldDescription>{INBOUND_COPY.description}</FieldDescription>
      </CardHeader>
      <form
        className="contents"
        onSubmit={(event) => {
          event.preventDefault();
          if (!form.state.isSubmitting) void form.handleSubmit();
        }}
      >
        <CardContent>
          <FieldGroup>
            <form.Field name="only_added_repositories">
              {(field) => (
                <Field orientation="horizontal">
                  <Switch
                    id={`${identifier}-repository-scope`}
                    aria-describedby={`${identifier}-repository-scope-hint`}
                    checked={field.state.value}
                    onCheckedChange={field.handleChange}
                  />
                  <FieldContent>
                    <FieldLabel htmlFor={`${identifier}-repository-scope`}>
                      {INBOUND_COPY.onlyAddedRepositories}
                    </FieldLabel>
                    <FieldDescription id={`${identifier}-repository-scope-hint`}>
                      {INBOUND_COPY.onlyAddedRepositoriesHint}
                    </FieldDescription>
                  </FieldContent>
                </Field>
              )}
            </form.Field>
            <form.Field name="shortcuts" mode="array">
              {(shortcuts) => (
                <FieldSet>
                  <FieldLegend>{INBOUND_COPY.shortcuts}</FieldLegend>
                  {/* Chromium lays out no boxes for a row when a paragraph is added beside it in the
                  update that changes the row, so the hints stay inside this paragraph. */}
                  <FieldDescription>
                    {INBOUND_COPY.shortcutHint}
                    {unlisted.map((agent) => (
                      <span key={agent} className="mt-2 block">
                        {INBOUND_COPY.noSuggestions[agent]}
                      </span>
                    ))}
                  </FieldDescription>
                  {shortcuts.state.value.map((_, index) => (
                    <FieldGroup key={index} className="gap-3">
                      <form.Field name={`shortcuts[${index}].agent`}>
                        {(field) => (
                          <Field>
                            <FieldLabel htmlFor={field.name}>{INBOUND_COPY.agent}</FieldLabel>
                            <Select
                              items={AGENT_NAMES}
                              value={field.state.value}
                              onValueChange={(agent) => {
                                if (agent && agent !== field.state.value) {
                                  form.setFieldValue(`shortcuts[${index}]`, (shortcut) =>
                                    withAgent(shortcut, agent),
                                  );
                                }
                              }}
                            >
                              <SelectTrigger id={field.name} className="w-full">
                                <SelectValue />
                              </SelectTrigger>
                              <SelectContent>
                                {SHORTCUT_AGENTS.map((agent) => (
                                  <SelectItem key={agent} value={agent}>
                                    {AGENT_NAMES[agent]}
                                  </SelectItem>
                                ))}
                              </SelectContent>
                            </Select>
                          </Field>
                        )}
                      </form.Field>
                      <form.Field name={`shortcuts[${index}].trigger`}>
                        {(field) => (
                          <Field>
                            <FieldLabel htmlFor={field.name}>{INBOUND_COPY.trigger}</FieldLabel>
                            <Input
                              id={field.name}
                              value={field.state.value}
                              onChange={(event) => field.handleChange(event.target.value)}
                            />
                          </Field>
                        )}
                      </form.Field>
                      <form.Subscribe
                        selector={(state) => state.values.shortcuts[index]?.agent ?? "codex"}
                      >
                        {(agent) => (
                          <>
                            <form.Field name={`shortcuts[${index}].model`}>
                              {(field) => (
                                <Field>
                                  <FieldLabel htmlFor={field.name}>{INBOUND_COPY.model}</FieldLabel>
                                  <Autocomplete
                                    id={field.name}
                                    value={field.state.value}
                                    options={modelSuggestions(agentModels(agents.data, agent))}
                                    showOptionsLabel={INBOUND_COPY.showModels[agent]}
                                    placeholder={INBOUND_COPY.currentDefault}
                                    onValueChange={field.handleChange}
                                  />
                                </Field>
                              )}
                            </form.Field>
                            <form.Field name={`shortcuts[${index}].effort`}>
                              {(field) => (
                                <Field>
                                  <FieldLabel htmlFor={field.name}>
                                    {INBOUND_COPY.effort}
                                  </FieldLabel>
                                  <form.Subscribe
                                    selector={(state) => state.values.shortcuts[index]?.model ?? ""}
                                  >
                                    {(selectedModel) => (
                                      <Autocomplete
                                        id={field.name}
                                        value={field.state.value}
                                        options={effortSuggestions(
                                          agentModels(agents.data, agent),
                                          selectedModel,
                                        )}
                                        showOptionsLabel={INBOUND_COPY.showEfforts[agent]}
                                        placeholder={INBOUND_COPY.currentDefault}
                                        onValueChange={field.handleChange}
                                      />
                                    )}
                                  </form.Subscribe>
                                </Field>
                              )}
                            </form.Field>
                          </>
                        )}
                      </form.Subscribe>
                      <Button
                        type="button"
                        variant="outline"
                        onClick={() => shortcuts.removeValue(index)}
                      >
                        {INBOUND_COPY.remove}
                      </Button>
                    </FieldGroup>
                  ))}
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() =>
                      shortcuts.pushValue({ trigger: "", agent: "codex", model: "", effort: "" })
                    }
                  >
                    {INBOUND_COPY.add}
                  </Button>
                </FieldSet>
              )}
            </form.Field>
            <form.Field name="poll_interval_seconds">
              {(field) => (
                <Field>
                  <FieldLabel htmlFor={field.name}>{INBOUND_COPY.pollInterval}</FieldLabel>
                  <Input
                    id={field.name}
                    type="number"
                    min={1}
                    max={4294967295}
                    step={1}
                    value={field.state.value}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                  <FieldDescription>{INBOUND_COPY.pollIntervalHint}</FieldDescription>
                </Field>
              )}
            </form.Field>
            <form.Field name="waiting_expiry_hours">
              {(field) => (
                <Field>
                  <FieldLabel htmlFor={field.name}>{INBOUND_COPY.waitingExpiry}</FieldLabel>
                  <Input
                    id={field.name}
                    type="number"
                    min={1}
                    max={4294967295}
                    step={1}
                    value={field.state.value}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                  <FieldDescription>{INBOUND_COPY.waitingExpiryHint}</FieldDescription>
                </Field>
              )}
            </form.Field>
            <form.Field name="retention_days">
              {(field) => (
                <Field>
                  <FieldLabel htmlFor={field.name}>{INBOUND_COPY.retention}</FieldLabel>
                  <Input
                    id={field.name}
                    type="number"
                    min={0}
                    max={4294967295}
                    step={1}
                    value={field.state.value}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                  <FieldDescription>{INBOUND_COPY.retentionHint}</FieldDescription>
                </Field>
              )}
            </form.Field>
            <form.Field name="feedback">
              {(field) => (
                <FieldSet>
                  <FieldLegend id={`${identifier}-feedback`}>{INBOUND_COPY.feedback}</FieldLegend>
                  <RadioGroup
                    aria-labelledby={`${identifier}-feedback`}
                    value={field.state.value}
                    onValueChange={field.handleChange}
                  >
                    {[
                      { value: "reactions", label: INBOUND_COPY.feedbackReactions },
                      { value: "footer", label: INBOUND_COPY.feedbackFooter },
                      { value: "off", label: INBOUND_COPY.feedbackOff },
                    ].map((option) => (
                      <Field key={option.value} orientation="horizontal">
                        <RadioGroupItem id={`${identifier}-${option.value}`} value={option.value} />
                        <FieldLabel htmlFor={`${identifier}-${option.value}`}>
                          {option.label}
                        </FieldLabel>
                      </Field>
                    ))}
                  </RadioGroup>
                  <FieldDescription>{INBOUND_COPY.feedbackHint}</FieldDescription>
                </FieldSet>
              )}
            </form.Field>
          </FieldGroup>
        </CardContent>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(submitting) => (
            <CardFooter className="justify-between gap-4">
              <p role="alert" className="text-destructive">
                {error}
              </p>
              <Button type="submit" loading={submitting}>
                {INBOUND_COPY.save}
              </Button>
            </CardFooter>
          )}
        </form.Subscribe>
      </form>
    </Card>
  );
}
