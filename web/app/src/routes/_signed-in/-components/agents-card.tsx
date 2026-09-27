import type { AgentStatus, RemoteControlOverview } from "@ezra/client";
import { getCodexRemoteControlLogOptions } from "@ezra/client/react-query.gen";
import { cn } from "cn";
import { EllipsisIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";
import { useCallback, useId, useRef, useState } from "react";

import { Waiting } from "@/components/sign-in-steps";
import { Badge } from "@/components/ui/badge";
import type { BadgeVariant } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
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
import { PAIRING_DESCRIPTIONS } from "@/content/pairing";
import { REMOTE_CONTROL_DESCRIPTIONS } from "@/content/remote-control";
import {
  useAgentActionError,
  useAgentActionPending,
  useAgentActions,
} from "@/hooks/use-agent-actions";
import { useCodexActions, useCodexPairing } from "@/hooks/use-codex-actions";
import { useNow } from "@/hooks/use-now";
import { menuOffersInstall, nextAgentStep } from "@/lib/agent-steps";
import type { AgentStep } from "@/lib/agent-steps";
import { codexPairable, codexRemoteView } from "@/lib/codex-remote";
import type { CodexFix, RemoteView } from "@/lib/codex-remote";
import { handOffFocus } from "@/lib/focus";
import { remoteControlSummary, serversRun } from "@/lib/remote-control";
import { signInEnd } from "@/lib/sign-in";
import type { SignInEnd } from "@/lib/sign-in";
import { downloadPercent, formatDateTime } from "@/lib/utils";

import { CodexSignInDialog } from "./codex-sign-in-dialog";
import { PairPhoneDialog } from "./pair-phone-dialog";
import { PairedPhonesDialog } from "./paired-phones-dialog";
import { ServerLogDialog } from "./server-log-dialog";
import { RestartServersDialog } from "./restart-servers-dialog";
import { UninstallAgentDialog } from "./uninstall-agent-dialog";

/** On a phone card the actions join the card's grid, so the menu sits beside the badge. */
const ACTIONS_LAYOUT = {
  row: {
    actions: "flex w-52 flex-col items-end gap-1",
    buttons: "flex items-center justify-end gap-1",
  },
  card: { actions: "contents", buttons: "contents" },
} as const;

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
  /** The agent's Remote cell. */
  remote: RemoteView;
  /** Whether a phone can pair with Codex. */
  pairable: boolean;
  /** Whether any of the agent's remote control servers runs. */
  serving: boolean;
}

/** Every agent with its install and sign-in actions: a table on wide screens, a list on phones. */
export function AgentsCard({ agents, remoteControl }: AgentsCardProps) {
  const now = useNow(CLOCK_MS);
  const pairable = codexPairable(remoteControl.codex);
  const remote = (status: AgentStatus): RemoteView =>
    status.agent === "claude"
      ? { label: remoteControlSummary(remoteControl) }
      : codexRemoteView(remoteControl.codex, status.installed_version);
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
              <TableHead className="w-52">
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
                pairable={pairable}
                serving={serversRun(status.agent, remoteControl)}
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
              pairable={pairable}
              serving={serversRun(status.agent, remoteControl)}
            />
          ))}
        </ul>
      </CardContent>
    </Card>
  );
}

function AgentRow({ status, end, remote, pairable, serving }: AgentProps) {
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
      <TableCell className="tabular-nums">
        <Remote view={remote} />
      </TableCell>
      <TableCell className="text-right tabular-nums">{facts.savedData}</TableCell>
      <TableCell>
        <AgentActions
          status={status}
          nameId={nameId}
          ending={Boolean(end)}
          remote={remote}
          pairable={pairable}
          serving={serving}
          layout="row"
        />
      </TableCell>
    </TableRow>
  );
}

function AgentListItem({ status, end, remote, pairable, serving }: AgentProps) {
  const nameId = useId();
  const facts = agentFacts(status);
  const state = agentState(status, end);
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-2 gap-y-3 py-4 first:pt-0 last:pb-0">
      <div className="flex items-center justify-between gap-3">
        <span id={nameId} className="font-medium">
          {AGENT_NAMES[status.agent]}
        </span>
        <Badge variant={state.variant}>{state.label}</Badge>
      </div>
      <dl className="col-span-full grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1">
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
        <dd className="tabular-nums">
          <Remote view={remote} />
        </dd>
        <dt className="text-muted-foreground">
          <SavedDataLabel />
        </dt>
        <dd className="tabular-nums">{facts.savedData}</dd>
      </dl>
      <AgentActions
        status={status}
        nameId={nameId}
        ending={Boolean(end)}
        remote={remote}
        pairable={pairable}
        serving={serving}
        layout="card"
      />
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

function Remote({ view }: { view: RemoteView }) {
  return (
    <>
      {view.label}
      {view.note ? (
        <span
          className={cn(
            "block max-w-64 text-xs whitespace-normal",
            view.note.tone === "destructive" ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {view.note.text}
        </span>
      ) : null}
    </>
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
  remote,
  pairable,
  serving,
  layout,
}: {
  status: AgentStatus;
  nameId: string;
  ending: boolean;
  remote: RemoteView;
  pairable: boolean;
  serving: boolean;
  layout: keyof typeof ACTIONS_LAYOUT;
}) {
  const { install, startSignIn, signOut } = useAgentActions(status.agent);
  const codex = useCodexActions();
  const pairing = useCodexPairing();
  const [logOpen, setLogOpen] = useState(false);
  const [phonesOpen, setPhonesOpen] = useState(false);
  const [uninstalling, setUninstalling] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [signingInFor, setSigningInFor] = useState<CodexFix>();
  const installPending = useAgentActionPending(status.agent, "install");
  const signInPending = useAgentActionPending(status.agent, "start_sign_in");
  const signOutPending = useAgentActionPending(status.agent, "sign_out");
  const retryPending = useAgentActionPending(status.agent, "retry_remote_control");
  const menuTrigger = useRef<HTMLButtonElement>(null);
  const stepButton = useRef<HTMLButtonElement>(null);
  const keepFocusInRow = useCallback((button: HTMLButtonElement | null) => {
    stepButton.current = button;
    return handOffFocus(button, () => menuTrigger.current);
  }, []);
  const name = AGENT_NAMES[status.agent];
  const installed = Boolean(status.installed_version);
  const installing = installPending || Boolean(status.install_progress);
  const step = nextAgentStep(
    status,
    installing,
    ending,
    remote.fix ?? (signInPending ? signingInFor : undefined),
  );
  const failure = useAgentActionError(
    status.agent,
    step === "try_again" ? undefined : "retry_remote_control",
  );
  const percent = status.install_progress ? downloadPercent(status.install_progress) : undefined;
  const path = { agent: status.agent };
  const installNow = () => install.mutate({ path });
  const signIn = () => {
    setSigningInFor(remote.fix);
    if (status.agent === "codex") {
      codex.signIn(status.logged_in);
    } else {
      startSignIn.mutate({ path });
    }
  };
  const steps: Record<AgentStep, { label: string; run: () => void; pending: boolean }> = {
    install: {
      label:
        installing && percent !== undefined
          ? AGENTS_DESCRIPTIONS.installing(percent)
          : installLabel(status),
      run: installNow,
      pending: installing,
    },
    sign_in: {
      label: status.logged_in ? AGENTS_DESCRIPTIONS.sign_in_again : AGENTS_DESCRIPTIONS.sign_in,
      run: signIn,
      pending: signInPending,
    },
    sign_in_with_chatgpt: {
      label: REMOTE_CONTROL_DESCRIPTIONS.sign_in_with_chatgpt,
      run: signIn,
      pending: signInPending,
    },
    try_again: {
      label: REMOTE_CONTROL_DESCRIPTIONS.try_again,
      run: () => codex.retry.mutate({}),
      pending: retryPending,
    },
  };
  const menuInstall = menuOffersInstall(status, step);
  return (
    <div className={ACTIONS_LAYOUT[layout].actions}>
      <div data-agent-actions={status.agent} className={ACTIONS_LAYOUT[layout].buttons}>
        {step ? (
          <Button
            ref={keepFocusInRow}
            size="sm"
            loading={steps[step].pending}
            aria-describedby={nameId}
            className="justify-self-start tabular-nums"
            onClick={steps[step].run}
          >
            {steps[step].label}
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
                  className="col-start-2 row-start-1"
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
              {serving ? (
                <DropdownMenuItem onClick={() => setRestarting(true)}>
                  {AGENTS_DESCRIPTIONS.restart_servers[status.agent]}
                </DropdownMenuItem>
              ) : null}
              {status.agent === "codex" ? (
                <>
                  <DropdownMenuItem
                    disabled={pairing.start.isPending}
                    onClick={() => void pairing.pairPhone(pairable)}
                  >
                    {PAIRING_DESCRIPTIONS.pair}
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setPhonesOpen(true)}>
                    {PAIRING_DESCRIPTIONS.phones}
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setLogOpen(true)}>
                    {REMOTE_CONTROL_DESCRIPTIONS.show_log}
                  </DropdownMenuItem>
                </>
              ) : null}
              <DropdownMenuSeparator />
              <DropdownMenuItem
                variant="destructive"
                disabled={installing}
                onClick={() => setUninstalling(true)}
              >
                {AGENTS_DESCRIPTIONS.uninstall}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
      </div>
      <RestartServersDialog
        agent={status.agent}
        open={restarting}
        onOpenChange={setRestarting}
        finalFocus={() => menuTrigger.current}
      />
      <UninstallAgentDialog
        agent={status.agent}
        open={uninstalling}
        onOpenChange={setUninstalling}
        finalFocus={() =>
          menuTrigger.current?.isConnected ? menuTrigger.current : stepButton.current
        }
      />
      {failure ? (
        <p role="alert" className="col-span-full whitespace-normal text-destructive">
          {failure}
        </p>
      ) : null}
      {status.agent === "codex" ? (
        <>
          <div role="status" className="col-span-full empty:sr-only">
            {pairing.start.isPending && !pairing.open ? (
              <Waiting label={PAIRING_DESCRIPTIONS.getting_code} />
            ) : null}
          </div>
          <CodexSignInDialog
            open={codex.confirmingSignIn}
            onOpenChange={codex.setConfirmingSignIn}
            onConfirm={codex.confirmSignIn}
          />
          <ServerLogDialog
            query={getCodexRemoteControlLogOptions()}
            title={REMOTE_CONTROL_DESCRIPTIONS.log_title(AGENT_NAMES.codex)}
            open={logOpen}
            onOpenChange={setLogOpen}
          />
          <PairPhoneDialog flow={pairing} remote={remote} pairable={pairable} />
          <PairedPhonesDialog open={phonesOpen} onOpenChange={setPhonesOpen} />
        </>
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
