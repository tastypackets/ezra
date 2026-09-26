import { Field as BaseField } from "@base-ui/react/field";
import { Input as BaseInput } from "@base-ui/react/input";
import { cn } from "cn";

export interface FieldProps {
  label: string;
  /** A line under the control about what to enter. */
  hint?: string;
  /** Shown instead of the control's own validity message. */
  error?: string | undefined;
  /** Hides the label visually while keeping it for screen readers. */
  hideLabel?: boolean;
  children: React.ReactNode;
  className?: string;
}

/** Label, hint and error wrapper for one control. */
export function Field({ label, hint, error, hideLabel = false, children, className }: FieldProps) {
  return (
    <BaseField.Root invalid={Boolean(error)} className={cn("flex flex-col gap-1", className)}>
      <BaseField.Label className={cn("font-medium text-ez-text-soft", hideLabel && "sr-only")}>
        {label}
      </BaseField.Label>
      {children}
      {hint ? (
        <BaseField.Description className="text-ez-muted">{hint}</BaseField.Description>
      ) : null}
      {error ? (
        <BaseField.Error match role="alert" className="text-[0.8125rem] text-ez-danger">
          {error}
        </BaseField.Error>
      ) : null}
    </BaseField.Root>
  );
}

export interface TextInputProps extends Omit<React.ComponentProps<typeof BaseInput>, "className"> {
  className?: string;
}

/** The text input look, 16px on phones so iOS Safari does not zoom in on focus. */
export const TEXT_INPUT_CLASS =
  "h-9 w-full rounded-md border border-ez-border-strong bg-ez-surface px-3 text-base text-ez-text shadow-ez-card outline-none focus:border-ez-accent focus:ring-3 focus:ring-ez-focus data-invalid:border-ez-danger sm:text-sm";

/** A text input that picks up its label from the surrounding `Field`. */
export function TextInput({ className, ...props }: TextInputProps) {
  return <BaseInput className={cn(TEXT_INPUT_CLASS, className)} {...props} />;
}
