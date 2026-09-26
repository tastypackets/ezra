import {
  chooseFolderSpawnModeMutation,
  chooseToServeFolderMutation,
  cloneRepositoryMutation,
  stopCloneMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toastManager } from "@/components/ui/toast";
import { SPAWN_MODES } from "@/content/folders";
import { clonesQueryOptions, foldersQueryOptions } from "@/queries/folder-queries";

/** Choosing whether the Claude app lists a folder and where its sessions work. Each refreshes the folders when it settles. */
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
  return { chooseToServe, chooseSpawnMode };
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
