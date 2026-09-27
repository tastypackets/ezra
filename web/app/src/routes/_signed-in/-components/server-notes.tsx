import type { RemoteControlStatus, ServerProblem } from "@ezra/client";
import { cn } from "cn";

import { Button } from "@/components/ui/button";
import { AGENT_NAMES } from "@/content/agents";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_PROBLEMS } from "@/content/remote-control";
import { useAgentActionPending, useAgentActions } from "@/hooks/use-agent-actions";
import { formatDateTime } from "@/lib/utils";

const FIXED_BY_SIGNING_IN: ServerProblem[] = ["sign_in", "not_enabled"];

export interface ServerNotesProps {
  status: RemoteControlStatus;
  /** Adds the server's last output, the sign-in button and the update deadline. */
  detailed?: boolean;
}

/** Why a server keeps stopping, and the Claude Code update it waits to restart on. */
export function ServerNotes({ status, detailed = false }: ServerNotesProps) {
  const stopped = status.state === "retrying" || status.state === "starting";
  const problem = stopped ? status.problem : undefined;
  return (
    <>
      {problem ? <ProblemNote problem={problem} withSignIn={detailed} /> : null}
      {detailed && stopped && status.last_error ? (
        <p className={cn(problem ? "text-muted-foreground" : "text-destructive")}>
          {REMOTE_CONTROL_DESCRIPTIONS.last_stop}:{" "}
          <span className="font-mono text-xs break-words whitespace-pre-wrap">
            {status.last_error}
          </span>
        </p>
      ) : null}
      {status.update ? (
        <p className="text-muted-foreground">
          {detailed
            ? REMOTE_CONTROL_DESCRIPTIONS.update_waiting(
                AGENT_NAMES.claude,
                status.update.version,
                formatDateTime(status.update.restart_by),
              )
            : REMOTE_CONTROL_DESCRIPTIONS.update_waiting_short(status.update.version)}
        </p>
      ) : null}
    </>
  );
}

function ProblemNote({ problem, withSignIn }: { problem: ServerProblem; withSignIn: boolean }) {
  const { startSignIn } = useAgentActions("claude");
  const signingIn = useAgentActionPending("claude", "start_sign_in");
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
      <p className="text-destructive">{SERVER_PROBLEMS[problem]}</p>
      {withSignIn && FIXED_BY_SIGNING_IN.includes(problem) ? (
        <Button
          size="sm"
          variant="outline"
          loading={signingIn}
          onClick={() => startSignIn.mutate({ path: { agent: "claude" } })}
        >
          {REMOTE_CONTROL_DESCRIPTIONS.sign_in_again}
        </Button>
      ) : null}
    </div>
  );
}
