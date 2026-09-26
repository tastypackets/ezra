import { Link } from "@tanstack/react-router";

import { StateCard } from "@/components/state-card";
import { buttonClassName } from "@/components/ui/button";
import { APP_DESCRIPTIONS } from "@/content/app";

export function NotFoundState() {
  return (
    <StateCard
      title={APP_DESCRIPTIONS.not_found_title}
      description={APP_DESCRIPTIONS.not_found_description}
      action={
        <Link to="/" className={buttonClassName({ variant: "primary" })}>
          {APP_DESCRIPTIONS.back_to_dashboard}
        </Link>
      }
    />
  );
}
