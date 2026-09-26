import type {
  CloneStatus,
  FolderStatus,
  RemoteControlOverview,
  RemoteControlStatus,
} from "@ezra/client";
import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { EllipsisIcon, ExternalLinkIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";
import { useId, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Hint } from "@/components/ui/hint";
import { Progress, ProgressLabel, ProgressValue } from "@/components/ui/progress";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { AGENT_NAMES } from "@/content/agents";
import { FOLDERS_DESCRIPTIONS } from "@/content/folders";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";
import { useCloneActions, useFolderActions } from "@/hooks/use-folder-actions";
import { SERVER_BADGES } from "@/lib/remote-control";
import { errorMessage } from "@/lib/utils";
import { agentsQueryOptions, isClaudeInstalled } from "@/queries/agent-queries";
import { clonesQueryOptions, foldersQueryOptions } from "@/queries/folder-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

import { ClaudeOptionsDialog } from "./claude-options-dialog";
import { CloneDialog } from "./clone-dialog";
import { DeleteFolderDialog } from "./delete-folder-dialog";
import { ServerLogDialog } from "./server-log-dialog";
import { ServerNotes } from "./server-notes";

/** /projects and the projects in it, the clones on their way, and which ones the Claude app lists. */
export function FoldersCard() {
  const folders = useQuery(foldersQueryOptions);
  const clones = useQuery(clonesQueryOptions);
  const { data: remoteControl } = useSuspenseQuery(remoteControlQueryOptions);
  const { data: claudeInstalled } = useSuspenseQuery({
    ...agentsQueryOptions,
    select: isClaudeInstalled,
  });
  const listed = folders.data ?? [];
  const cloning = clones.data ?? [];
  const taken = {
    folders: listed.map((folder) => folder.name),
    cloning: cloning.filter((clone) => !clone.error).map((clone) => clone.name),
  };
  const device = remoteControl.device;
  return (
    <Card>
      <CardHeader>
        <CardTitle>{FOLDERS_DESCRIPTIONS.title}</CardTitle>
        {claudeInstalled && remoteControl.projects.state === "running" ? (
          <CardDescription>
            {device
              ? REMOTE_CONTROL_DESCRIPTIONS.running_hint(device)
              : REMOTE_CONTROL_DESCRIPTIONS.running_hint_without_device}
          </CardDescription>
        ) : null}
        <CardAction>
          <CloneDialog taken={taken} claudeInstalled={claudeInstalled} />
        </CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {claudeInstalled || cloning.length > 0 || listed.length > 0 ? (
          <ul className="flex flex-col divide-y">
            {claudeInstalled ? <ProjectsRow overview={remoteControl} /> : null}
            {cloning.map((clone) => (
              <CloneRow key={`clone:${clone.name}`} clone={clone} />
            ))}
            {listed.map((folder) => (
              <FolderRow
                key={folder.name}
                folder={folder}
                server={claudeInstalled ? remoteControl.folders[folder.name] : undefined}
                claudeInstalled={claudeInstalled}
              />
            ))}
          </ul>
        ) : null}
        {folders.isPending ? (
          <div className="flex justify-center text-muted-foreground">
            <Spinner />
          </div>
        ) : folders.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(folders.error)}
          </p>
        ) : listed.length === 0 && cloning.length === 0 ? (
          <p className="text-muted-foreground">{FOLDERS_DESCRIPTIONS.empty}</p>
        ) : null}
      </CardContent>
    </Card>
  );
}

/** The server for /projects, which Settings turns on and off. */
function ProjectsRow({ overview }: { overview: RemoteControlOverview }) {
  const status = overview.projects;
  const [logOpen, setLogOpen] = useState(false);
  const name = FOLDERS_DESCRIPTIONS.projects_path;
  return (
    <li className="flex items-start gap-2 py-3 first:pt-0 last:pb-0 sm:items-center">
      <div className="flex min-w-0 flex-1 flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-col gap-0.5">
          <span className="font-medium">{FOLDERS_DESCRIPTIONS.projects}</span>
          <span className="text-muted-foreground">{name}</span>
          {status.state === "off" ? (
            <Link
              to="/settings"
              className="w-fit text-muted-foreground underline underline-offset-4 hover:text-foreground"
            >
              {REMOTE_CONTROL_DESCRIPTIONS.off_hint}
            </Link>
          ) : status.state === "waiting" ? (
            <p className="text-muted-foreground">{REMOTE_CONTROL_DESCRIPTIONS.waiting_hint}</p>
          ) : null}
          <ServerNotes status={status} detailed />
        </div>
        <ClaudeCodeGroup name={name} server={status} />
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={FOLDERS_DESCRIPTIONS.more_actions(name)}
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
      <ServerLogDialog open={logOpen} onOpenChange={setLogOpen} />
    </li>
  );
}

function CloneRow({ clone }: { clone: CloneStatus }) {
  const { stopClone } = useCloneActions();
  const failed = Boolean(clone.error);
  return (
    <li className="flex flex-col gap-2 py-3 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex flex-wrap items-baseline gap-x-3">
          <span className="min-w-0 font-medium break-all">{clone.name}</span>
          <span className="min-w-0 truncate text-muted-foreground">{clone.repository}</span>
        </div>
        {clone.error ? (
          <p className="text-destructive">
            {FOLDERS_DESCRIPTIONS.clone_failed}:{" "}
            <span className="font-mono text-xs break-words whitespace-pre-wrap">{clone.error}</span>
          </p>
        ) : (
          <Progress value={clone.percent} className="max-w-sm gap-1">
            <ProgressLabel className="font-normal text-muted-foreground">
              {FOLDERS_DESCRIPTIONS.cloning}
            </ProgressLabel>
            <ProgressValue />
          </Progress>
        )}
        {stopClone.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(stopClone.error)}
          </p>
        ) : null}
      </div>
      <Button
        variant="outline"
        size="sm"
        className="self-start sm:self-center"
        loading={stopClone.isPending}
        aria-label={
          failed
            ? FOLDERS_DESCRIPTIONS.dismiss_label(clone.name)
            : FOLDERS_DESCRIPTIONS.stop_clone_label(clone.name)
        }
        onClick={() => stopClone.mutate({ path: { name: clone.name } })}
      >
        {failed ? FOLDERS_DESCRIPTIONS.dismiss : FOLDERS_DESCRIPTIONS.stop_clone}
      </Button>
    </li>
  );
}

interface FolderRowProps {
  folder: FolderStatus;
  server?: RemoteControlStatus;
  claudeInstalled: boolean;
}

function FolderRow({ folder, server, claudeInstalled }: FolderRowProps) {
  const { chooseToServe } = useFolderActions();
  const [deleting, setDeleting] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [logOpen, setLogOpen] = useState(false);
  const detail = folder.git ? folder.git.repository : FOLDERS_DESCRIPTIONS.not_git;
  const failure = chooseToServe.error;
  const switchLabelId = useId();
  return (
    <li className="flex items-start gap-2 py-3 first:pt-0 last:pb-0 sm:items-center">
      <div className="flex min-w-0 flex-1 flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-col gap-0.5">
          <div className="flex flex-wrap items-baseline gap-x-3">
            <span className="min-w-0 font-medium break-all">{folder.name}</span>
            {folder.git?.branch ? (
              <span className="min-w-0 font-mono text-xs break-all text-muted-foreground">
                {folder.git.branch}
              </span>
            ) : null}
            {folder.git?.worktrees ? (
              <span className="text-xs text-muted-foreground">
                {FOLDERS_DESCRIPTIONS.worktrees(folder.git.worktrees)}
              </span>
            ) : null}
          </div>
          {detail ? <span className="truncate text-muted-foreground">{detail}</span> : null}
          {server ? <ServerNotes status={server} /> : null}
          {failure ? (
            <p role="alert" className="text-destructive">
              {errorMessage(failure)}
            </p>
          ) : null}
        </div>
        {claudeInstalled ? (
          <ClaudeCodeGroup name={folder.name} server={server}>
            <Switch
              checked={chooseToServe.isPending ? chooseToServe.variables.body.serve : folder.serve}
              disabled={chooseToServe.isPending}
              onCheckedChange={(serve) =>
                chooseToServe.mutate({ path: { name: folder.name }, body: { serve } })
              }
              aria-labelledby={switchLabelId}
              className="ml-auto sm:ml-0"
            />
            <span id={switchLabelId} className="sr-only">
              {FOLDERS_DESCRIPTIONS.serve_label(folder.name)}
            </span>
          </ClaudeCodeGroup>
        ) : null}
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={FOLDERS_DESCRIPTIONS.more_actions(folder.name)}
            />
          }
        >
          <EllipsisIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          {claudeInstalled ? (
            <>
              <DropdownMenuItem onClick={() => setOptionsOpen(true)}>
                {FOLDERS_DESCRIPTIONS.claude_options}
              </DropdownMenuItem>
              {server ? (
                <DropdownMenuItem onClick={() => setLogOpen(true)}>
                  {REMOTE_CONTROL_DESCRIPTIONS.show_log}
                </DropdownMenuItem>
              ) : null}
              <DropdownMenuSeparator />
            </>
          ) : null}
          <DropdownMenuItem variant="destructive" onClick={() => setDeleting(true)}>
            {FOLDERS_DESCRIPTIONS.delete}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <ClaudeOptionsDialog folder={folder} open={optionsOpen} onOpenChange={setOptionsOpen} />
      <DeleteFolderDialog
        folder={folder}
        served={server?.state === "running" || server?.state === "starting"}
        open={deleting}
        onOpenChange={setDeleting}
      />
      {server ? (
        <ServerLogDialog folder={folder.name} open={logOpen} onOpenChange={setLogOpen} />
      ) : null}
    </li>
  );
}

interface ClaudeCodeGroupProps {
  /** The folder the server serves, for the link's name. */
  name: string;
  server?: RemoteControlStatus;
  children?: React.ReactNode;
}

/** A row's Claude Code side: its Remote Control server's sessions, memory, state and link. */
function ClaudeCodeGroup({ name, server, children }: ClaudeCodeGroupProps) {
  const labelId = useId();
  return (
    <div
      role="group"
      aria-labelledby={labelId}
      className="flex min-h-8 flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border px-2.5 py-1"
    >
      <span id={labelId} className="text-xs font-medium text-muted-foreground">
        {AGENT_NAMES.claude}
      </span>
      {server?.usage ? (
        <span className="flex gap-3 text-xs text-muted-foreground tabular-nums">
          {REMOTE_CONTROL_DESCRIPTIONS.sessions(server.usage.sessions, server.usage.capacity)}
          <Hint content={REMOTE_CONTROL_DESCRIPTIONS.memory_hint}>
            {prettyBytes(server.usage.memory_bytes)}
          </Hint>
        </span>
      ) : null}
      {server ? (
        <Badge variant={SERVER_BADGES[server.state]}>{SERVER_STATES[server.state]}</Badge>
      ) : null}
      {server?.url ? (
        <a
          href={server.url}
          target="_blank"
          rel="noopener noreferrer"
          aria-label={FOLDERS_DESCRIPTIONS.open(name)}
          className={buttonVariants({ variant: "ghost", size: "icon-xs" })}
        >
          <ExternalLinkIcon />
        </a>
      ) : null}
      {children}
    </div>
  );
}
