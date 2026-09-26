import type { AgentStatus } from "@ezra/client";
import { cn } from "cn";

import { Badge } from "@/components/ui/badge";
import type { BadgeProps } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader } from "@/components/ui/card";
import { Cell, HeaderCell, Row, Table } from "@/components/ui/table";
import { Tooltip } from "@/components/ui/tooltip";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import {
  useAgentActionError,
  useAgentActionPending,
  useAgentActions,
} from "@/hooks/use-agent-actions";
import { downloadPercent, formatBytes } from "@/lib/utils";

export interface AgentsCardProps {
  agents: AgentStatus[];
}

/** Every agent with its install and sign-in actions: a table on wide screens, a list on phones. */
export function AgentsCard({ agents }: AgentsCardProps) {
  return (
    <Card>
      <CardHeader title={AGENTS_DESCRIPTIONS.title} description={AGENTS_DESCRIPTIONS.description} />
      <Table className="hidden lg:block">
        <thead>
          <tr>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_agent}</HeaderCell>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_status}</HeaderCell>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_version}</HeaderCell>
            <HeaderCell>{AGENTS_DESCRIPTIONS.column_account}</HeaderCell>
            <HeaderCell numeric>{AGENTS_DESCRIPTIONS.column_sessions}</HeaderCell>
            <HeaderCell numeric>
              <SavedDataLabel />
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
      <ul className="divide-y divide-ez-border lg:hidden">
        {agents.map((status) => (
          <AgentListItem key={status.agent} status={status} />
        ))}
      </ul>
    </Card>
  );
}

function AgentRow({ status }: { status: AgentStatus }) {
  const facts = agentFacts(status);
  const state = agentState(status);
  return (
    <Row>
      <Cell className="font-medium">{AGENT_NAMES[status.agent]}</Cell>
      <Cell>
        <Badge tone={state.tone}>{state.label}</Badge>
      </Cell>
      <Cell className={cn(status.installed_version && "font-mono")}>{facts.version}</Cell>
      <Cell>{facts.account}</Cell>
      <Cell numeric>{facts.sessions}</Cell>
      <Cell numeric>{facts.savedData}</Cell>
      <Cell numeric>
        <AgentActions status={status} className="items-end" />
      </Cell>
    </Row>
  );
}

function AgentListItem({ status }: { status: AgentStatus }) {
  const facts = agentFacts(status);
  const state = agentState(status);
  return (
    <li className="flex flex-col gap-3 px-5 py-4">
      <div className="flex items-center justify-between gap-3">
        <span className="font-medium">{AGENT_NAMES[status.agent]}</span>
        <Badge tone={state.tone}>{state.label}</Badge>
      </div>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1">
        <dt className="text-ez-muted">{AGENTS_DESCRIPTIONS.column_version}</dt>
        <dd className={cn(status.installed_version && "font-mono")}>{facts.version}</dd>
        <dt className="text-ez-muted">{AGENTS_DESCRIPTIONS.column_account}</dt>
        <dd className="truncate">{facts.account}</dd>
        <dt className="text-ez-muted">{AGENTS_DESCRIPTIONS.column_sessions}</dt>
        <dd className="tabular-nums">{facts.sessions}</dd>
        <dt className="text-ez-muted">
          <SavedDataLabel />
        </dt>
        <dd className="tabular-nums">{facts.savedData}</dd>
      </dl>
      <AgentActions status={status} className="items-start" />
    </li>
  );
}

function SavedDataLabel() {
  return (
    <Tooltip content={AGENTS_DESCRIPTIONS.saved_data_hint}>
      {AGENTS_DESCRIPTIONS.column_saved_data}
    </Tooltip>
  );
}

function AgentActions({ status, className }: { status: AgentStatus; className: string }) {
  const { install, startSignIn, signOut } = useAgentActions(status.agent);
  const installPending = useAgentActionPending(status.agent, "install");
  const signInPending = useAgentActionPending(status.agent, "start_sign_in");
  const signOutPending = useAgentActionPending(status.agent, "sign_out");
  const failure = useAgentActionError(status.agent);
  const installed = Boolean(status.installed_version);
  const percent = status.install_progress ? downloadPercent(status.install_progress) : undefined;
  const installing = installPending || Boolean(status.install_progress);
  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <div className="flex flex-wrap gap-2">
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
          <Button size="sm" loading={signOutPending} onClick={() => signOut.mutate()}>
            {AGENTS_DESCRIPTIONS.sign_out}
          </Button>
        ) : null}
        {installed && !status.logged_in && !status.login_prompt ? (
          <Button
            size="sm"
            variant="primary"
            loading={signInPending}
            onClick={() => startSignIn.mutate()}
          >
            {AGENTS_DESCRIPTIONS.sign_in}
          </Button>
        ) : null}
      </div>
      {failure ? (
        <p role="alert" className="max-w-sm text-[0.8125rem] whitespace-normal text-ez-danger">
          {failure}
        </p>
      ) : null}
    </div>
  );
}

function agentFacts(status: AgentStatus) {
  return {
    version: status.installed_version ?? "—",
    account: status.account ?? (status.logged_in ? AGENTS_DESCRIPTIONS.unavailable : "—"),
    sessions: status.session_count ?? AGENTS_DESCRIPTIONS.unavailable,
    savedData:
      typeof status.config_disk_bytes === "number"
        ? formatBytes(status.config_disk_bytes)
        : AGENTS_DESCRIPTIONS.unavailable,
  };
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
