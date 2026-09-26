import {
  changePasswordMutation,
  endOtherSessionsMutation,
  regenerateCertificateMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toastManager } from "@/components/ui/toast";
import { MANAGER_DESCRIPTIONS } from "@/content/manager";
import { managerQueryOptions } from "@/queries/manager-queries";

/** Changing the password, signing out other sessions and regenerating the certificate. */
export function useManagerActions() {
  const queryClient = useQueryClient();
  const refreshManager = () =>
    queryClient.invalidateQueries({ queryKey: managerQueryOptions.queryKey });

  const changePassword = useMutation({
    ...changePasswordMutation(),
    onSuccess: () => toastManager.add({ title: MANAGER_DESCRIPTIONS.password_changed }),
    onSettled: refreshManager,
  });
  const endOtherSessions = useMutation({
    ...endOtherSessionsMutation(),
    onSuccess: () => toastManager.add({ title: MANAGER_DESCRIPTIONS.other_sessions_ended }),
    onSettled: refreshManager,
  });
  const regenerateCertificate = useMutation({
    ...regenerateCertificateMutation(),
    onSuccess: () => toastManager.add({ title: MANAGER_DESCRIPTIONS.regenerated }),
    onSettled: refreshManager,
  });
  return { changePassword, endOtherSessions, regenerateCertificate };
}
