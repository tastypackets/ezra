import { Spinner } from "@/components/ui/spinner";

/** Shown while a route waits on the manager for longer than a moment. */
export function LoadingState() {
  return (
    <div className="flex justify-center pt-24 text-muted-foreground">
      <Spinner className="size-6" />
    </div>
  );
}
