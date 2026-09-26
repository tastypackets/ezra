import { Spinner } from "@/components/ui/spinner";
import { APP_DESCRIPTIONS } from "@/content/app";

/** Shown while a route waits on the manager for longer than a moment. */
export function LoadingState() {
  return (
    <div role="status" className="flex justify-center pt-24 text-ez-muted">
      <Spinner className="size-6" />
      <span className="sr-only">{APP_DESCRIPTIONS.loading}</span>
    </div>
  );
}
