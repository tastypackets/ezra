import type { AgentStatus } from "@ezra/client";
import { cn } from "cn";

import { Badge } from "@/components/ui/badge";
import type { BadgeProps } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader } from "@/components/ui/card";
import { Cell, HeaderCell, Row, Table } from "@/components/ui/table";
import { Tooltip } from "@/components/ui/tooltip";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import { useAgentActionError, useAgentActions } from "@/hooks/use-agent-actions";
import { downloadPercent, formatBytes } from "@/lib/utils";

export interface AgentsCardProps {
  agents: AgentStatus[];
}

/** Every agent in one table, with its install and sign-in actions. */
export function AgentsCard({ agents }: AgentsCardProps) {
  return (
    <Card>
      <CardHeader title={AGENTS_DESCRIPTIONS.title} description={AGENTS_DESCRIPTIONS.description} />
      <Table>
        <thead>
          <tr>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_agent}</HeaderCell>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_status}</HeaderCell>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_version}</HeaderCell>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_account}</HeaderCell>
            <HeaderCell numeric>{AGENTS_DESCRIPTIONS.column_sessions}</HeaderCell>
            <HeaderCell numeric>
              <Tooltip content={AGENTS_DESCRIPTIONS.saved_data_hint}>
                {AGENTS_DESCRIPTIONS.column_saved_data}
              </Tooltip>
            </HeaderCell>
            <HeaderCell>
              <span className="sr-only">{AGENTS_DESCRIPTIONS.column_actions}</span>
            </HeaderCell>
          </tr>
        </thead>
        <tbody>
          {agents.map((status) => (
            <AgentRow key={status.agent} status={status} />
          ))}
        </tbody>
      </Table>
    </Card>
  );
}

function AgentRow({ status }: { status: AgentStatus }) {
  const installed = Boolean(status.installed_version);
  const state = agentState(status);
  return (
    <Row>
      <Cell className="font-medium">{AGENT_NAMES[status.agent]}</Cell>
      <Cell>
        <Badge tone={state.tone}>{state.label}</Badge>
      </Cell>
      <Cell className={cn(installed && "font-mono")}>{status.installed_version ?? "—"}</Cell>
      <Cell>{status.account ?? (status.logged_in ? AGENTS_DESCRIPTIONS.unavailable : "—")}</Cell>
      <Cell numeric>{status.session_count ?? AGENTS_DESCRIPTIONS.unavailable}</Cell>
      <Cell numeric>
        {typeof status.config_disk_bytes === "number"
          ? formatBytes(status.config_disk_bytes)
          : AGENTS_DESCRIPTIONS.unavailable}
      </Cell>
      <Cell numeric>
        <AgentActions status={status} />
      </Cell>
    </Row>
  );
}

function AgentActions({ status }: { status: AgentStatus }) {
  const { install, startSignIn, signOut } = useAgentActions(status.agent);
  const installed = Boolean(status.installed_version);
  const percent = status.install_progress ? downloadPercent(status.install_progress) : undefined;
  const installing = install.isPending || Boolean(status.install_progress);
  const failure = useAgentActionError(status.agent);
  return (
    <div className="inline-flex flex-col items-end gap-1">
      <div className="inline-flex gap-2">
        <Button
          size="sm"
          variant={installed ? "secondary" : "primary"}
          loading={installing}
          onClick={() => install.mutate()}
        >
          {installing && percent !== undefined
            ? `${percent}%`
            : installed
              ? AGENTS_DESCRIPTIONS.update
              : AGENTS_DESCRIPTIONS.install}
        </Button>
        {installed && status.logged_in ? (
          <Button size="sm" loading={signOut.isPending} onClick={() => signOut.mutate()}>
            {AGENTS_DESCRIPTIONS.sign_out}
          </Button>
        ) : null}
        {installed && !status.logged_in && !status.login_prompt ? (
          <Button
            size="sm"
            variant="primary"
            loading={startSignIn.isPending}
            onClick={() => startSignIn.mutate()}
          >
            {AGENTS_DESCRIPTIONS.sign_in}
          </Button>
        ) : null}
      </div>
      {failure ? (
        <p role="alert" className="text-[0.8125rem] whitespace-normal text-ez-danger">
          {failure}
        </p>
      ) : null}
    </div>
  );
}

function agentState(status: AgentStatus): { label: string; tone: BadgeProps["tone"] } {
  if (!status.installed_version) {
    return { label: AGENTS_DESCRIPTIONS.status_not_installed, tone: "neutral" };
  }
  if (status.logged_in) {
    return { label: AGENTS_DESCRIPTIONS.status_signed_in, tone: "good" };
  }
  if (status.login_prompt) {
    return { label: AGENTS_DESCRIPTIONS.status_signing_in, tone: "pending" };
  }
  return { label: AGENTS_DESCRIPTIONS.status_signed_out, tone: "neutral" };
}
