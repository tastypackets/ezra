import {
  chooseFolderSpawnModeMutation,
  chooseToServeFolderMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toastManager } from "@/components/ui/toast";
import { SPAWN_MODES } from "@/content/folders";
import { foldersQueryOptions } from "@/queries/folder-queries";

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
