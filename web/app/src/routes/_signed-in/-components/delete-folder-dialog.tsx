import type { FolderStatus, UnsavedWork } from "@ezra/client";
import { getUnsavedWorkOptions } from "@ezra/client/react-query.gen";
import { useQuery } from "@tanstack/react-query";

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
import { Spinner } from "@/components/ui/spinner";
import { DELETE_FOLDER_DESCRIPTIONS } from "@/content/folders";
import { useFolderActions } from "@/hooks/use-folder-actions";
import { errorMessage } from "@/lib/utils";

export interface DeleteFolderDialogProps {
  folder: FolderStatus;
  /** Whether the folder has a Remote Control server that deleting stops. */
  served: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** Confirms deleting a folder, listing the work that exists only in it. */
export function DeleteFolderDialog({
  folder,
  served,
  open,
  onOpenChange,
}: DeleteFolderDialogProps) {
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <DeleteFolderConfirmation
          folder={folder}
          served={served}
          onDeleted={() => onOpenChange(false)}
        />
      </AlertDialogContent>
    </AlertDialog>
  );
}

function DeleteFolderConfirmation({
  folder,
  served,
  onDeleted,
}: Pick<DeleteFolderDialogProps, "folder" | "served"> & { onDeleted: () => void }) {
  const { deleteFolder } = useFolderActions();
  const unsaved = useQuery({
    ...getUnsavedWorkOptions({ path: { name: folder.name } }),
    enabled: Boolean(folder.git),
    staleTime: 0,
    retry: false,
  });
  const checking = Boolean(folder.git) && unsaved.isPending;
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>{DELETE_FOLDER_DESCRIPTIONS.title(folder.name)}</AlertDialogTitle>
        <AlertDialogDescription>
          {DELETE_FOLDER_DESCRIPTIONS.description(folder.name)}
        </AlertDialogDescription>
      </AlertDialogHeader>
      {checking ? (
        <p className="flex items-center gap-2 text-muted-foreground">
          <Spinner />
          {DELETE_FOLDER_DESCRIPTIONS.checking}
        </p>
      ) : null}
      {unsaved.isError ? (
        <p className="text-destructive">
          {DELETE_FOLDER_DESCRIPTIONS.check_failed}:{" "}
          <span className="font-mono text-xs break-words whitespace-pre-wrap">
            {errorMessage(unsaved.error)}
          </span>
        </p>
      ) : null}
      {unsaved.data ? <UnsavedWorkList work={unsaved.data} /> : null}
      {served ? <p>{DELETE_FOLDER_DESCRIPTIONS.server}</p> : null}
      {deleteFolder.isError ? (
        <p role="alert" className="text-destructive">
          {errorMessage(deleteFolder.error)}
        </p>
      ) : null}
      <AlertDialogFooter>
        <AlertDialogCancel>{DELETE_FOLDER_DESCRIPTIONS.cancel}</AlertDialogCancel>
        <AlertDialogAction
          variant="destructive"
          disabled={checking}
          loading={deleteFolder.isPending}
          onClick={() =>
            deleteFolder.mutate({ path: { name: folder.name } }, { onSuccess: onDeleted })
          }
        >
          {DELETE_FOLDER_DESCRIPTIONS.confirm}
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

function UnsavedWorkList({ work }: { work: UnsavedWork }) {
  const warnings = [
    work.uncommitted_changes > 0
      ? DELETE_FOLDER_DESCRIPTIONS.uncommitted_changes(work.uncommitted_changes)
      : undefined,
    work.unpushed_commits > 0
      ? DELETE_FOLDER_DESCRIPTIONS.unpushed_commits(work.unpushed_commits)
      : undefined,
    work.stashes > 0 ? DELETE_FOLDER_DESCRIPTIONS.stashes(work.stashes) : undefined,
  ].filter((warning) => warning !== undefined);
  if (warnings.length === 0) {
    return null;
  }
  return (
    <div className="flex flex-col gap-1 text-destructive">
      <p className="font-medium">{DELETE_FOLDER_DESCRIPTIONS.only_here}</p>
      <ul className="ml-4 list-disc">
        {warnings.map((warning) => (
          <li key={warning}>{warning}</li>
        ))}
      </ul>
    </div>
  );
}
