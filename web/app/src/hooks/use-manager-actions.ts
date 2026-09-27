import {
  changePasswordMutation,
  regenerateCertificateMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toast } from "@/components/ui/toast";
import { MANAGER_DESCRIPTIONS } from "@/content/manager";
import { managerQueryOptions } from "@/queries/manager-queries";

/** Changing the password and regenerating the certificate. */
export function useManagerActions() {
  const queryClient = useQueryClient();
  const refreshManager = () =>
    queryClient.invalidateQueries({ queryKey: managerQueryOptions.queryKey });

  const changePassword = useMutation({
    ...changePasswordMutation(),
    onSuccess: () => toast.add({ title: MANAGER_DESCRIPTIONS.password_changed }),
    onSettled: refreshManager,
  });
  const regenerateCertificate = useMutation({
    ...regenerateCertificateMutation(),
    onSuccess: () => toast.add({ title: MANAGER_DESCRIPTIONS.regenerated }),
    onSettled: refreshManager,
  });
  return { changePassword, regenerateCertificate };
}
