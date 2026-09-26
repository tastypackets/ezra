import { Field as BaseField } from "@base-ui/react/field";
import { Input as BaseInput } from "@base-ui/react/input";
import { cn } from "cn";

export interface FieldProps {
  label: string;
  /** Shown instead of the control's own validity message. */
  error?: string | undefined;
  /** Hides the label visually while keeping it for screen readers. */
  hideLabel?: boolean;
  children: React.ReactNode;
  className?: string;
}

/** Label and error wrapper for one control. */
export function Field({ label, error, hideLabel = false, children, className }: FieldProps) {
  return (
    <BaseField.Root className={cn("flex flex-col gap-1", className)}>
      <BaseField.Label className={cn("font-medium text-ez-text-soft", hideLabel && "sr-only")}>
        {label}
      </BaseField.Label>
      {children}
      {error ? <p className="text-[0.8125rem] text-ez-danger">{error}</p> : null}
    </BaseField.Root>
  );
}

export interface TextInputProps extends Omit<React.ComponentProps<typeof BaseInput>, "className"> {
  className?: string;
}

/** A text input that picks up its label from the surrounding `Field`. */
export function TextInput({ className, ...props }: TextInputProps) {
  return (
    <BaseInput
      className={cn(
        "h-9 w-full rounded-md border border-ez-border-strong bg-ez-surface px-3 text-ez-text shadow-ez-card outline-none focus:border-ez-accent focus:ring-3 focus:ring-ez-focus",
        className,
      )}
      {...props}
    />
  );
}
