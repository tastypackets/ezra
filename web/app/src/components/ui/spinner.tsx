import { cn } from "cn";
import { Loader2Icon } from "lucide-react";

import { APP_DESCRIPTIONS } from "@/content/app";

function Spinner({ className, ...props }: React.ComponentProps<"svg">) {
  return (
    <Loader2Icon
      data-slot="spinner"
      role="status"
      aria-label={APP_DESCRIPTIONS.loading}
      className={cn("size-4 animate-spin", className)}
      {...props}
    />
  );
}

export { Spinner };
