import type {
  Agent,
  GetSettingsFileError,
  ParseProblem,
  SettingsFileFormat,
  SettingsFileText,
} from "@ezra/client";
import { updateSettingsFileMutation } from "@ezra/client/react-query.gen";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { lazy, Suspense, useCallback, useId, useRef, useState } from "react";

import type { CodeEditorHandle } from "@/components/code-editor";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { CardContent, CardHeader } from "@/components/ui/card";
import { FieldError } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import { AGENTS_DESCRIPTIONS } from "@/content/agents";
import { APP_DESCRIPTIONS } from "@/content/app";
import { SETTINGS_FILE_DESCRIPTIONS, SETTINGS_FILES } from "@/content/settings-file";
import { useAgentActions } from "@/hooks/use-agent-actions";
import { serversRun } from "@/lib/remote-control";
import { capitalized, errorMessage } from "@/lib/utils";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";
import { settingsFileQueryOptions } from "@/queries/settings-file-queries";

const CodeEditor = lazy(() => import("@/components/code-editor"));

/** Whether a problem found in the browser blocks Save, only where its check is the agent's own parser. */
const BROWSER_CHECK_BLOCKS_SAVE: Record<SettingsFileFormat, boolean> = {
  json: true,
  toml: false,
};

interface Draft {
  /** The file as the page opened it, or as it was last saved. */
  opened: SettingsFileText;
  /** The text last put into the editor from outside, by opening, Revert or a reload. */
  loaded: { text: string };
  text: string;
  /** The problem the browser's own check marks. */
  marked?: ParseProblem;
  /** The server's answer for `text`, a problem or none. */
  judged?: { text: string; problem?: ParseProblem };
}

export type SettingsFileDraft = ReturnType<typeof useSettingsFileDraft>;

/** A draft of `file` as the page first shows it. */
function opening(file: SettingsFileText): Draft {
  return { opened: file, loaded: { text: file.text }, text: file.text };
}

/**
 * An agent's own settings file as the user edits it, saved by the card's one Save button only when
 * it parses and nothing changed it meanwhile. Nothing loads while `enabled` is false.
 */
export function useSettingsFileDraft(agent: Agent, enabled: boolean) {
  const queryClient = useQueryClient();
  const { queryKey } = settingsFileQueryOptions(agent);
  const query = useQuery({ ...settingsFileQueryOptions(agent), enabled });
  const file = enabled ? query.data : undefined;
  const editor = useRef<CodeEditorHandle>(null);
  const [draft, setDraft] = useState<Draft>();
  const { data: serving = false } = useQuery({
    ...remoteControlQueryOptions,
    enabled,
    select: (overview) => serversRun(agent, overview),
  });
  const { restartServers } = useAgentActions(agent);
  const [savedWhileServing, setSavedWhileServing] = useState(false);
  const write = useMutation({
    ...updateSettingsFileMutation(),
    onSuccess: async (saved) => {
      setSavedWhileServing(serving);
      setDraft((current) => current && { ...current, opened: saved, judged: { text: saved.text } });
      await queryClient.cancelQueries({ queryKey });
      queryClient.setQueryData(queryKey, saved);
    },
    onError: async (error, { body }) => {
      if ("line" in error) {
        setDraft(
          (current) => current && { ...current, judged: { text: body.text, problem: error } },
        );
        editor.current?.focus();
      } else {
        await queryClient.invalidateQueries({ queryKey });
      }
    },
  });

  if (file && !draft) {
    setDraft(opening(file));
  }
  const dirty = Boolean(draft && draft.text !== draft.opened.text);
  const changedOnDisk = Boolean(
    file && draft && !write.isPending && file.version !== draft.opened.version,
  );
  if (file && draft && changedOnDisk && draft.text === file.text) {
    setDraft({ ...draft, opened: file });
  } else if (file && draft && changedOnDisk && !dirty) {
    setDraft(opening(file));
  }
  const verdict = draft?.judged?.text === draft?.text ? draft?.judged : undefined;
  const blocked =
    dirty &&
    (verdict
      ? Boolean(verdict.problem)
      : Boolean(file && BROWSER_CHECK_BLOCKS_SAVE[file.format] && draft?.marked));
  const notice = !file
    ? undefined
    : changedOnDisk
      ? SETTINGS_FILE_DESCRIPTIONS.changed_on_disk(file.path)
      : query.error
        ? errorMessage(query.error)
        : write.isError && !("line" in write.error)
          ? errorMessage(write.error)
          : undefined;

  return {
    agent,
    file,
    readError: query.error,
    retrying: query.isFetching,
    retry: () => void query.refetch(),
    editor,
    draft,
    dirty,
    blocked,
    changedOnDisk,
    verdict,
    notice,
    saving: write.isPending,
    /** Set after a save while the agent's servers run, which still use the old file. */
    offerRestart: savedWhileServing && serving,
    restarting: restartServers.isPending,
    restart: () =>
      restartServers.mutate({ path: { agent } }, { onSuccess: () => setSavedWhileServing(false) }),
    edit: (text: string) => {
      setSavedWhileServing(false);
      setDraft((current) => current && { ...current, text, judged: undefined });
      if (write.isError) {
        write.reset();
      }
    },
    mark: (marked: ParseProblem | undefined) =>
      setDraft((current) => current && { ...current, marked }),
    /** Saves the edited text over the version the page last read. Throws when the save fails. */
    save: async () => {
      if (!file || !draft) {
        return;
      }
      await write.mutateAsync({
        path: { agent },
        body: { text: draft.text, version: file.version },
      });
    },
    /** Puts the file as it is on disk back into the editor. */
    revert: () => {
      write.reset();
      if (file) {
        setDraft(opening(file));
      }
    },
  };
}

/** The file's part of its agent's card: a title, the editor, and a restart once saved. */
export function SettingsFileSection({ draft }: { draft: SettingsFileDraft }) {
  const titleId = useId();
  const { agent, file, readError } = draft;
  return (
    <section aria-labelledby={titleId} className="flex flex-col gap-(--card-spacing)">
      <CardHeader>
        <h3 id={titleId} className="text-base font-medium">
          {SETTINGS_FILES[agent].title}
        </h3>
      </CardHeader>
      {file && draft.draft ? (
        <SettingsFileEditor draft={draft} labelledBy={titleId} />
      ) : readError ? (
        <ReadFailed error={readError} retrying={draft.retrying} retry={draft.retry} />
      ) : (
        <CardContent className="flex justify-center py-8 text-muted-foreground">
          <Spinner className="size-6" />
        </CardContent>
      )}
      {draft.offerRestart ? (
        <CardContent className="flex flex-wrap items-center gap-3">
          <p className="text-muted-foreground">{SETTINGS_FILES[agent].still_running}</p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            loading={draft.restarting}
            onClick={draft.restart}
          >
            {AGENTS_DESCRIPTIONS.restart_servers[agent]}
          </Button>
        </CardContent>
      ) : null}
    </section>
  );
}

function ReadFailed({
  error,
  retrying,
  retry,
}: {
  error: GetSettingsFileError;
  retrying: boolean;
  retry: () => void;
}) {
  return (
    <CardContent className="flex flex-col items-start gap-3">
      <p role="alert" className="text-destructive">
        {errorMessage(error)}
      </p>
      <Button type="button" variant="outline" loading={retrying} onClick={retry}>
        {APP_DESCRIPTIONS.retry}
      </Button>
    </CardContent>
  );
}

function SettingsFileEditor({
  draft,
  labelledBy,
}: {
  draft: SettingsFileDraft;
  labelledBy: string;
}) {
  const problemId = useId();
  const { file, editor, verdict } = draft;
  const state = draft.draft;
  if (!file || !state) {
    return null;
  }
  return (
    <CardContent className="flex flex-col gap-2">
      <Suspense
        fallback={
          <div className="flex justify-center py-8 text-muted-foreground">
            <Spinner className="size-6" />
          </div>
        }
      >
        <CodeEditor
          ref={editor}
          format={file.format}
          loaded={state.loaded}
          serverVerdict={verdict}
          onChange={draft.edit}
          onProblem={draft.mark}
          aria-labelledby={labelledBy}
          aria-describedby={problemId}
        />
      </Suspense>
      {state.marked ? (
        <FieldError id={problemId}>
          {SETTINGS_FILE_DESCRIPTIONS.problem({
            ...state.marked,
            error: capitalized(state.marked.error),
          })}
        </FieldError>
      ) : null}
      <LeaveGuard path={file.path} dirty={draft.dirty} />
    </CardContent>
  );
}

/** Asks before leaving the page while `dirty`. */
function LeaveGuard({ path, dirty }: { path: string; dirty: boolean }) {
  const shouldBlockFn = useCallback(() => dirty, [dirty]);
  const leaving = useBlocker({ shouldBlockFn, enableBeforeUnload: dirty, withResolver: true });
  return (
    <AlertDialog
      open={leaving.status === "blocked"}
      onOpenChange={(open) => {
        if (!open) {
          leaving.reset?.();
        }
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{SETTINGS_FILE_DESCRIPTIONS.leave_title}</AlertDialogTitle>
          <AlertDialogDescription>
            {SETTINGS_FILE_DESCRIPTIONS.leave_description(path)}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{SETTINGS_FILE_DESCRIPTIONS.stay}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={() => leaving.proceed?.()}>
            {SETTINGS_FILE_DESCRIPTIONS.leave}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
