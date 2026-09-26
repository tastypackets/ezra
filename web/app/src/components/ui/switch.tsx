import { Switch as BaseSwitch } from "@base-ui/react/switch";
import { useId } from "react";

export interface SwitchProps {
  label: string;
  description?: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}

/** An on and off setting with its label and a line on what it does. */
export function Switch({ label, description, checked, onCheckedChange }: SwitchProps) {
  const labelId = useId();
  const descriptionId = useId();
  return (
    <label className="flex cursor-pointer items-start gap-3">
      <BaseSwitch.Root
        checked={checked}
        onCheckedChange={onCheckedChange}
        aria-labelledby={labelId}
        aria-describedby={description ? descriptionId : undefined}
        className="mt-0.5 flex h-5 w-9 flex-none rounded-full bg-ez-border-strong p-0.5 transition-colors outline-none focus-visible:ring-3 focus-visible:ring-ez-focus data-checked:bg-ez-accent"
      >
        <BaseSwitch.Thumb className="size-4 rounded-full bg-white shadow-ez-card transition-transform data-checked:translate-x-4" />
      </BaseSwitch.Root>
      <span className="flex flex-col gap-0.5">
        <span id={labelId} className="font-medium">
          {label}
        </span>
        {description ? (
          <span id={descriptionId} className="text-ez-muted">
            {description}
          </span>
        ) : null}
      </span>
    </label>
  );
}
