import type { AgentStatus, RemoteControlOverview } from "@ezra/client";
import { cn } from "cn";
import { EllipsisIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";
import { useCallback, useId, useRef } from "react";

import { Badge } from "@/components/ui/badge";
import type { BadgeVariant } from "@/components/ui/badge";
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
import { useNow } from "@/hooks/use-now";
import { menuOffersInstall, nextAgentStep } from "@/lib/agent-steps";
import { handOffFocus } from "@/lib/focus";
import { remoteControlSummary } from "@/lib/remote-control";
import { signInEnd } from "@/lib/sign-in";
import type { SignInEnd } from "@/lib/sign-in";
import { downloadPercent, formatDateTime } from "@/lib/utils";

/** How often the sign-in warning is checked against the clock. */
const CLOCK_MS = 60_000;

export interface AgentsCardProps {
  agents: AgentStatus[];
  remoteControl: RemoteControlOverview;
}

interface AgentProps {
  status: AgentStatus;
  /** Set once the sign-in end is close enough to warn about. */
  end: SignInEnd | undefined;
  /** Remote Control in a few words, for the agents that have it. */
  remote: string;
}

/** Every agent with its install and sign-in actions: a table on wide screens, a list on phones. */
export function AgentsCard({ agents, remoteControl }: AgentsCardProps) {
  const now = useNow(CLOCK_MS);
  const remote = (status: AgentStatus) =>
    status.agent === "claude" ? remoteControlSummary(remoteControl) : "—";
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
              <TableHead>
                <RemoteLabel />
              </TableHead>
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
              <AgentRow
                key={status.agent}
                status={status}
                end={signInEnd(status, now)}
                remote={remote(status)}
              />
            ))}
          </TableBody>
        </Table>
      </CardContent>
      <CardContent className="lg:hidden">
        <ul className="flex flex-col divide-y">
          {agents.map((status) => (
            <AgentListItem
              key={status.agent}
              status={status}
              end={signInEnd(status, now)}
              remote={remote(status)}
            />
          ))}
        </ul>
      </CardContent>
    </Card>
  );
}

function AgentRow({ status, end, remote }: AgentProps) {
  const nameId = useId();
  const facts = agentFacts(status);
  const state = agentState(status, end);
  return (
    <TableRow>
      <TableHead scope="row" id={nameId}>
        {AGENT_NAMES[status.agent]}
      </TableHead>
      <TableCell>
        <Badge variant={state.variant}>{state.label}</Badge>
      </TableCell>
      <TableCell className={cn(status.installed_version && "font-mono")}>{facts.version}</TableCell>
      <TableCell>
        {facts.account}
        <SignInEndNote end={end} />
      </TableCell>
      <TableCell className="tabular-nums">{remote}</TableCell>
      <TableCell className="text-right tabular-nums">{facts.savedData}</TableCell>
      <TableCell>
        <AgentActions status={status} nameId={nameId} ending={Boolean(end)} className="items-end" />
      </TableCell>
    </TableRow>
  );
}

function AgentListItem({ status, end, remote }: AgentProps) {
  const nameId = useId();
  const facts = agentFacts(status);
  const state = agentState(status, end);
  return (
    <li className="flex flex-col gap-3 py-4 first:pt-0 last:pb-0">
      <div className="flex items-center justify-between gap-3">
        <span id={nameId} className="font-medium">
          {AGENT_NAMES[status.agent]}
        </span>
        <Badge variant={state.variant}>{state.label}</Badge>
      </div>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1">
        <dt className="text-muted-foreground">{AGENTS_DESCRIPTIONS.column_version}</dt>
        <dd className={cn(status.installed_version && "font-mono")}>{facts.version}</dd>
        <dt className="text-muted-foreground">{AGENTS_DESCRIPTIONS.column_account}</dt>
        <dd className="truncate">
          {facts.account}
          <SignInEndNote end={end} />
        </dd>
        <dt className="text-muted-foreground">
          <RemoteLabel />
        </dt>
        <dd className="tabular-nums">{remote}</dd>
        <dt className="text-muted-foreground">
          <SavedDataLabel />
        </dt>
        <dd className="tabular-nums">{facts.savedData}</dd>
      </dl>
      <AgentActions status={status} nameId={nameId} ending={Boolean(end)} className="items-start" />
    </li>
  );
}

function SignInEndNote({ end }: { end: SignInEnd | undefined }) {
  if (!end) {
    return null;
  }
  const when = formatDateTime(end.at);
  return (
    <span className={cn("block text-xs", end.ended ? "text-destructive" : "text-warning")}>
      {end.ended ? AGENTS_DESCRIPTIONS.sign_in_ended(when) : AGENTS_DESCRIPTIONS.sign_in_ends(when)}
    </span>
  );
}

function RemoteLabel() {
  return <Hint content={AGENTS_DESCRIPTIONS.remote_hint}>{AGENTS_DESCRIPTIONS.column_remote}</Hint>;
}

function SavedDataLabel() {
  return (
    <Hint content={AGENTS_DESCRIPTIONS.saved_data_hint}>
      {AGENTS_DESCRIPTIONS.column_saved_data}
    </Hint>
  );
}

/** The next step as one button, and the rest in a menu. */
function AgentActions({
  status,
  nameId,
  ending,
  className,
}: {
  status: AgentStatus;
  nameId: string;
  ending: boolean;
  className: string;
}) {
  const { install, startSignIn, signOut } = useAgentActions(status.agent);
  const installPending = useAgentActionPending(status.agent, "install");
  const signInPending = useAgentActionPending(status.agent, "start_sign_in");
  const signOutPending = useAgentActionPending(status.agent, "sign_out");
  const failure = useAgentActionError(status.agent);
  const menuTrigger = useRef<HTMLButtonElement>(null);
  const keepFocusInRow = useCallback(
    (button: HTMLButtonElement | null) => handOffFocus(button, () => menuTrigger.current),
    [],
  );
  const name = AGENT_NAMES[status.agent];
  const installed = Boolean(status.installed_version);
  const installing = installPending || Boolean(status.install_progress);
  const step = nextAgentStep(status, installing, ending);
  const percent = status.install_progress ? downloadPercent(status.install_progress) : undefined;
  const path = { agent: status.agent };
  const installNow = () => install.mutate({ path });
  const menuInstall = menuOffersInstall(status, step);
  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <div data-agent-actions={status.agent} className="flex items-center gap-1">
        {step ? (
          <Button
            ref={keepFocusInRow}
            size="sm"
            loading={step === "install" ? installing : signInPending}
            aria-describedby={nameId}
            onClick={step === "install" ? installNow : () => startSignIn.mutate({ path })}
          >
            {step === "sign_in"
              ? status.logged_in
                ? AGENTS_DESCRIPTIONS.sign_in_again
                : AGENTS_DESCRIPTIONS.sign_in
              : installing && percent !== undefined
                ? AGENTS_DESCRIPTIONS.installing(percent)
                : installLabel(status)}
          </Button>
        ) : null}
        {installed ? (
          <DropdownMenu>
            <DropdownMenuTrigger
              ref={menuTrigger}
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
            <DropdownMenuContent align="end" className="w-auto">
              {menuInstall ? (
                <DropdownMenuItem disabled={installing} onClick={installNow}>
                  {installLabel(status)}
                </DropdownMenuItem>
              ) : null}
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
    savedData:
      typeof status.config_disk_bytes === "number"
        ? prettyBytes(status.config_disk_bytes)
        : AGENTS_DESCRIPTIONS.unavailable,
  };
}

function agentState(
  status: AgentStatus,
  end: SignInEnd | undefined,
): { label: string; variant: BadgeVariant } {
  if (!status.installed_version) {
    return { label: AGENTS_DESCRIPTIONS.status_not_installed, variant: "secondary" };
  }
  if (status.logged_in) {
    if (end) {
      return end.ended
        ? { label: AGENTS_DESCRIPTIONS.status_sign_in_ended, variant: "destructive" }
        : { label: AGENTS_DESCRIPTIONS.status_sign_in_ending, variant: "warning" };
    }
    return { label: AGENTS_DESCRIPTIONS.status_signed_in, variant: "success" };
  }
  if (status.login_prompt) {
    return { label: AGENTS_DESCRIPTIONS.status_signing_in, variant: "warning" };
  }
  return { label: AGENTS_DESCRIPTIONS.status_signed_out, variant: "secondary" };
}
