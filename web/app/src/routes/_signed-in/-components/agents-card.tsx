import type { AgentStatus } from "@ezra/client";
import { cn } from "cn";
import { EllipsisIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";

import { Badge } from "@/components/ui/badge";
import type { badgeVariants } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Hint } from "@/components/ui/hint";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import {
  useAgentActionError,
  useAgentActionPending,
  useAgentActions,
} from "@/hooks/use-agent-actions";
import { downloadPercent } from "@/lib/utils";

type BadgeVariant = NonNullable<Parameters<typeof badgeVariants>[0]>["variant"];

export interface AgentsCardProps {
  agents: AgentStatus[];
}

/** Every agent with its install and sign-in actions: a table on wide screens, a list on phones. */
export function AgentsCard({ agents }: AgentsCardProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{AGENTS_DESCRIPTIONS.title}</CardTitle>
      </CardHeader>
      <CardContent className="hidden lg:block">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{AGENTS_DESCRIPTIONS.column_agent}</TableHead>
              <TableHead>{AGENTS_DESCRIPTIONS.column_status}</TableHead>
              <TableHead>{AGENTS_DESCRIPTIONS.column_version}</TableHead>
              <TableHead>{AGENTS_DESCRIPTIONS.column_account}</TableHead>
              <TableHead className="text-right">{AGENTS_DESCRIPTIONS.column_sessions}</TableHead>
              <TableHead className="text-right">
                <SavedDataLabel />
              </TableHead>
              <TableHead>
                <span className="sr-only">{AGENTS_DESCRIPTIONS.column_actions}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {agents.map((status) => (
              <AgentRow key={status.agent} status={status} />
            ))}
          </TableBody>
        </Table>
      </CardContent>
      <CardContent className="lg:hidden">
        <ul className="flex flex-col divide-y">
          {agents.map((status) => (
            <AgentListItem key={status.agent} status={status} />
          ))}
        </ul>
      </CardContent>
    </Card>
  );
}

function AgentRow({ status }: { status: AgentStatus }) {
  const facts = agentFacts(status);
  const state = agentState(status);
  return (
    <TableRow>
      <TableCell className="font-medium">{AGENT_NAMES[status.agent]}</TableCell>
      <TableCell>
        <Badge variant={state.variant}>{state.label}</Badge>
      </TableCell>
      <TableCell className={cn(status.installed_version && "font-mono")}>{facts.version}</TableCell>
      <TableCell>{facts.account}</TableCell>
      <TableCell className="text-right tabular-nums">{facts.sessions}</TableCell>
      <TableCell className="text-right tabular-nums">{facts.savedData}</TableCell>
      <TableCell>
        <AgentActions status={status} className="items-end" />
      </TableCell>
    </TableRow>
  );
}

function AgentListItem({ status }: { status: AgentStatus }) {
  const facts = agentFacts(status);
  const state = agentState(status);
  return (
    <li className="flex flex-col gap-3 py-4 first:pt-0 last:pb-0">
      <div className="flex items-center justify-between gap-3">
        <span className="font-medium">{AGENT_NAMES[status.agent]}</span>
        <Badge variant={state.variant}>{state.label}</Badge>
      </div>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1">
        <dt className="text-muted-foreground">{AGENTS_DESCRIPTIONS.column_version}</dt>
        <dd className={cn(status.installed_version && "font-mono")}>{facts.version}</dd>
        <dt className="text-muted-foreground">{AGENTS_DESCRIPTIONS.column_account}</dt>
        <dd className="truncate">{facts.account}</dd>
        <dt className="text-muted-foreground">{AGENTS_DESCRIPTIONS.column_sessions}</dt>
        <dd className="tabular-nums">{facts.sessions}</dd>
        <dt className="text-muted-foreground">
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
    <Hint content={AGENTS_DESCRIPTIONS.saved_data_hint}>
      {AGENTS_DESCRIPTIONS.column_saved_data}
    </Hint>
  );
}

/** The one next step as a button, and the rest in a menu. */
function AgentActions({ status, className }: { status: AgentStatus; className: string }) {
  const { install, startSignIn, signOut } = useAgentActions(status.agent);
  const installPending = useAgentActionPending(status.agent, "install");
  const signInPending = useAgentActionPending(status.agent, "start_sign_in");
  const signOutPending = useAgentActionPending(status.agent, "sign_out");
  const failure = useAgentActionError(status.agent);
  const name = AGENT_NAMES[status.agent];
  const installed = Boolean(status.installed_version);
  const installing = installPending || Boolean(status.install_progress);
  const percent = status.install_progress ? downloadPercent(status.install_progress) : undefined;
  const path = { agent: status.agent };
  const installNow = () => install.mutate({ path });
  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <div className="flex items-center gap-1">
        {installing || !installed || status.available_update ? (
          <Button size="sm" loading={installing} onClick={installNow}>
            {installing && percent !== undefined ? `${percent}%` : installLabel(status)}
          </Button>
        ) : null}
        {installed && !installing && !status.logged_in && !status.login_prompt ? (
          <Button size="sm" loading={signInPending} onClick={() => startSignIn.mutate({ path })}>
            {AGENTS_DESCRIPTIONS.sign_in}
          </Button>
        ) : null}
        {installed ? (
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button
                  variant="ghost"
                  size="icon-sm"
                  loading={signOutPending}
                  aria-label={AGENTS_DESCRIPTIONS.more_actions(name)}
                />
              }
            >
              {signOutPending ? null : <EllipsisIcon />}
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem disabled={installing} onClick={installNow}>
                {AGENTS_DESCRIPTIONS.check_for_update}
              </DropdownMenuItem>
              {status.logged_in ? (
                <DropdownMenuItem onClick={() => signOut.mutate({ path })}>
                  {AGENTS_DESCRIPTIONS.sign_out}
                </DropdownMenuItem>
              ) : null}
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
      </div>
      {failure ? (
        <p role="alert" className="max-w-sm whitespace-normal text-destructive">
          {failure}
        </p>
      ) : null}
    </div>
  );
}

function installLabel(status: AgentStatus): string {
  if (!status.installed_version) {
    return AGENTS_DESCRIPTIONS.install;
  }
  return status.available_update
    ? AGENTS_DESCRIPTIONS.update_to(status.available_update)
    : AGENTS_DESCRIPTIONS.check_for_update;
}

function agentFacts(status: AgentStatus) {
  return {
    version: status.installed_version ?? "—",
    account: status.account ?? (status.logged_in ? AGENTS_DESCRIPTIONS.unavailable : "—"),
    sessions: status.session_count ?? AGENTS_DESCRIPTIONS.unavailable,
    savedData:
      typeof status.config_disk_bytes === "number"
        ? prettyBytes(status.config_disk_bytes)
        : AGENTS_DESCRIPTIONS.unavailable,
  };
}

function agentState(status: AgentStatus): { label: string; variant: BadgeVariant } {
  if (!status.installed_version) {
    return { label: AGENTS_DESCRIPTIONS.status_not_installed, variant: "secondary" };
  }
  if (status.logged_in) {
    return { label: AGENTS_DESCRIPTIONS.status_signed_in, variant: "success" };
  }
  if (status.login_prompt) {
    return { label: AGENTS_DESCRIPTIONS.status_signing_in, variant: "warning" };
  }
  return { label: AGENTS_DESCRIPTIONS.status_signed_out, variant: "secondary" };
}
