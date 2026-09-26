import type { RemoteControlOverview, RemoteControlStatus, ServerState } from "@ezra/client";
import { Link } from "@tanstack/react-router";
import { EllipsisIcon, ExternalLinkIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";
import { useState } from "react";

import { Badge } from "@/components/ui/badge";
import type { badgeVariants } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";

import { ServerLogDialog } from "./server-log-dialog";
import { ServerNotes } from "./server-notes";

type BadgeVariant = NonNullable<Parameters<typeof badgeVariants>[0]>["variant"];

export const SERVER_BADGES: Record<ServerState, BadgeVariant> = {
  off: "secondary",
  waiting: "secondary",
  starting: "warning",
  running: "success",
  retrying: "warning",
  stopping: "warning",
};

/** Claude Code's Remote Control server on /projects: whether it runs, and where to continue. */
export function RemoteControlCard({ overview }: { overview: RemoteControlOverview }) {
  const status = overview.projects;
  const [logOpen, setLogOpen] = useState(false);
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {REMOTE_CONTROL_DESCRIPTIONS.title}
          <Badge variant={SERVER_BADGES[status.state]}>{SERVER_STATES[status.state]}</Badge>
        </CardTitle>
        <CardAction className="flex items-center gap-1">
          {status.url ? (
            <a
              href={status.url}
              target="_blank"
              rel="noopener noreferrer"
              className={buttonVariants({ variant: "outline", size: "sm" })}
            >
              {REMOTE_CONTROL_DESCRIPTIONS.open}
              <ExternalLinkIcon data-icon="inline-end" />
            </a>
          ) : null}
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={REMOTE_CONTROL_DESCRIPTIONS.more_actions}
                />
              }
            >
              <EllipsisIcon />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onClick={() => setLogOpen(true)}>
                {REMOTE_CONTROL_DESCRIPTIONS.show_log}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        <StateHint status={status} device={overview.device} />
        {status.usage ? (
          <p className="text-muted-foreground">
            {REMOTE_CONTROL_DESCRIPTIONS.usage(
              status.usage.sessions,
              status.usage.capacity,
              prettyBytes(status.usage.memory_bytes),
            )}
          </p>
        ) : null}
        <ServerNotes status={status} detailed />
      </CardContent>
      <ServerLogDialog open={logOpen} onOpenChange={setLogOpen} />
    </Card>
  );
}

function StateHint({ status, device }: { status: RemoteControlStatus; device?: string | null }) {
  switch (status.state) {
    case "off":
      return (
        <p className="text-muted-foreground">
          <Link to="/settings" className="underline underline-offset-4 hover:text-foreground">
            {REMOTE_CONTROL_DESCRIPTIONS.off_hint}
          </Link>
        </p>
      );
    case "running":
      return (
        <p className="text-muted-foreground">
          {device
            ? REMOTE_CONTROL_DESCRIPTIONS.running_hint(device)
            : REMOTE_CONTROL_DESCRIPTIONS.running_hint_without_device}
        </p>
      );
    case "waiting":
      return <p className="text-muted-foreground">{REMOTE_CONTROL_DESCRIPTIONS.waiting_hint}</p>;
    case "starting":
      return status.problem ? null : (
        <p className="text-muted-foreground">{REMOTE_CONTROL_DESCRIPTIONS.starting_hint}</p>
      );
    default:
      return null;
  }
}
