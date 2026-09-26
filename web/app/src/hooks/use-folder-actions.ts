import {
  chooseFolderSpawnModeMutation,
  chooseToServeFolderMutation,
  cloneRepositoryMutation,
  deleteFolderMutation,
  stopCloneMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toastManager } from "@/components/ui/toast";
import { DELETE_FOLDER_DESCRIPTIONS, SPAWN_MODES } from "@/content/folders";
import { clonesQueryOptions, foldersQueryOptions } from "@/queries/folder-queries";

/** Choosing whether the Claude app lists a folder and where its sessions work, and deleting it. Each refreshes the folders. */
export function useFolderActions() {
  const queryClient = useQueryClient();
  const refreshFolders = () =>
    queryClient.invalidateQueries({ queryKey: foldersQueryOptions.queryKey });
  const chooseToServe = useMutation({
    ...chooseToServeFolderMutation(),
    onSettled: refreshFolders,
  });
  const chooseSpawnMode = useMutation({
    ...chooseFolderSpawnModeMutation(),
    onSuccess: (_saved, { path, body }) =>
      toastManager.add({ title: SPAWN_MODES[body.spawn].saved(path.name) }),
    onSettled: refreshFolders,
  });
  const deleteFolder = useMutation({
    ...deleteFolderMutation(),
    onSuccess: (_, { path }) => {
      toastManager.add({ title: DELETE_FOLDER_DESCRIPTIONS.deleted(path.name) });
    },
    onSettled: refreshFolders,
  });
  return { chooseToServe, chooseSpawnMode, deleteFolder };
}

/** Starting a clone, and stopping or dismissing one. Each refreshes the clones. */
export function useCloneActions() {
  const queryClient = useQueryClient();
  const refreshClones = () =>
    queryClient.invalidateQueries({ queryKey: clonesQueryOptions.queryKey });
  const startClone = useMutation({ ...cloneRepositoryMutation(), onSettled: refreshClones });
  const stopClone = useMutation({ ...stopCloneMutation(), onSettled: refreshClones });
  return { startClone, stopClone };
}
