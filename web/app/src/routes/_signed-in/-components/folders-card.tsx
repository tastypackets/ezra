import type { CloneStatus, FolderStatus, RemoteControlStatus } from "@ezra/client";
import { useQuery } from "@tanstack/react-query";
import { EllipsisIcon, ExternalLinkIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";
import { useId, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Hint } from "@/components/ui/hint";
import { Label } from "@/components/ui/label";
import { Progress, ProgressLabel, ProgressValue } from "@/components/ui/progress";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { FOLDERS_DESCRIPTIONS } from "@/content/folders";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";
import { useCloneActions, useFolderActions } from "@/hooks/use-folder-actions";
import { errorMessage } from "@/lib/utils";
import { clonesQueryOptions, foldersQueryOptions } from "@/queries/folder-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

import { ClaudeOptionsDialog } from "./claude-options-dialog";
import { CloneDialog } from "./clone-dialog";
import { DeleteFolderDialog } from "./delete-folder-dialog";
import { SERVER_BADGES } from "./remote-control-card";
import { ServerLogDialog } from "./server-log-dialog";
import { ServerNotes } from "./server-notes";

/** The projects agents work in, the clones on their way, and which ones the Claude app lists. */
export function FoldersCard() {
  const folders = useQuery(foldersQueryOptions);
  const clones = useQuery(clonesQueryOptions);
  const remoteControl = useQuery(remoteControlQueryOptions);
  const cloning = clones.data ?? [];
  const taken = {
    folders: (folders.data ?? []).map((folder) => folder.name),
    cloning: cloning.filter((clone) => !clone.error).map((clone) => clone.name),
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle>{FOLDERS_DESCRIPTIONS.title}</CardTitle>
        <CardAction>
          <CloneDialog taken={taken} />
        </CardAction>
      </CardHeader>
      <CardContent>
        {folders.isPending ? (
          <div className="flex justify-center text-muted-foreground">
            <Spinner />
          </div>
        ) : folders.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(folders.error)}
          </p>
        ) : folders.data.length === 0 && cloning.length === 0 ? (
          <p className="text-muted-foreground">{FOLDERS_DESCRIPTIONS.empty}</p>
        ) : (
          <ul className="flex flex-col divide-y">
            {cloning.map((clone) => (
              <CloneRow key={`clone:${clone.name}`} clone={clone} />
            ))}
            {folders.data.map((folder) => (
              <FolderRow
                key={folder.name}
                folder={folder}
                server={remoteControl.data?.folders[folder.name]}
              />
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
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

function FolderRow({ folder, server }: { folder: FolderStatus; server?: RemoteControlStatus }) {
  const { chooseToServe } = useFolderActions();
  const [deleting, setDeleting] = useState(false);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const detail = folder.git ? folder.git.repository : FOLDERS_DESCRIPTIONS.not_git;
  const failure = chooseToServe.error;
  const switchId = useId();
  const switchLabelId = useId();
  const [logOpen, setLogOpen] = useState(false);
  return (
    <li className="flex flex-col gap-2 py-3 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:justify-between">
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
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 sm:flex-none sm:flex-nowrap">
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
            aria-label={FOLDERS_DESCRIPTIONS.open(folder.name)}
            className={buttonVariants({ variant: "ghost", size: "icon-sm" })}
          >
            <ExternalLinkIcon />
          </a>
        ) : null}
        <div className="flex items-center gap-2">
          <Switch
            id={switchId}
            checked={chooseToServe.isPending ? chooseToServe.variables.body.serve : folder.serve}
            disabled={chooseToServe.isPending}
            onCheckedChange={(serve) =>
              chooseToServe.mutate({ path: { name: folder.name }, body: { serve } })
            }
            aria-labelledby={switchLabelId}
          />
          <span id={switchLabelId} className="sr-only">
            {FOLDERS_DESCRIPTIONS.serve_label(folder.name)}
          </span>
          <Label htmlFor={switchId} className="text-muted-foreground">
            {FOLDERS_DESCRIPTIONS.serve}
          </Label>
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
            <DropdownMenuItem onClick={() => setOptionsOpen(true)}>
              {FOLDERS_DESCRIPTIONS.claude_options}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            {server ? (
              <DropdownMenuItem onClick={() => setLogOpen(true)}>
                {REMOTE_CONTROL_DESCRIPTIONS.show_log}
              </DropdownMenuItem>
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
      </div>
      {server ? (
        <ServerLogDialog folder={folder.name} open={logOpen} onOpenChange={setLogOpen} />
      ) : null}
    </li>
  );
}
