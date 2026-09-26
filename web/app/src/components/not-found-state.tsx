import { Link } from "@tanstack/react-router";

import { StateCard } from "@/components/state-card";
import { buttonVariants } from "@/components/ui/button";
import { APP_DESCRIPTIONS } from "@/content/app";

export function NotFoundState() {
  return (
    <StateCard
      title={APP_DESCRIPTIONS.not_found_title}
      action={
        <Link to="/" className={buttonVariants()}>
          {APP_DESCRIPTIONS.back_to_dashboard}
        </Link>
      }
    />
  );
}
