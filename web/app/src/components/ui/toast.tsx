import { Toast } from "@base-ui/react/toast";
import { XIcon } from "lucide-react";

/** The app's toasts, which code outside components can add to. */
const toastManager = Toast.createToastManager();

function Toaster({ closeLabel, children }: { closeLabel: string; children: React.ReactNode }) {
  return (
    <Toast.Provider toastManager={toastManager}>
      {children}
      <Toast.Portal>
        <Toast.Viewport className="fixed right-4 bottom-4 z-50 flex w-[calc(100vw-2rem)] flex-col gap-2 outline-none sm:w-80">
          <ToastList closeLabel={closeLabel} />
        </Toast.Viewport>
      </Toast.Portal>
    </Toast.Provider>
  );
}

function ToastList({ closeLabel }: { closeLabel: string }) {
  const { toasts } = Toast.useToastManager();
  return toasts.map((toast) => (
    <Toast.Root
      key={toast.id}
      toast={toast}
      className="flex items-start gap-3 rounded-lg bg-popover p-3 text-popover-foreground shadow-md ring-1 ring-foreground/10 transition-[opacity,translate] duration-200 data-ending-style:opacity-0 data-limited:hidden data-starting-style:translate-y-2 data-starting-style:opacity-0"
    >
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <Toast.Title className="font-medium" />
        <Toast.Description className="text-muted-foreground" />
      </div>
      <Toast.Close
        aria-label={closeLabel}
        className="rounded-md p-0.5 text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        <XIcon className="size-4" />
      </Toast.Close>
    </Toast.Root>
  ));
}

export { Toaster, toastManager };
