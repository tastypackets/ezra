import { Radio } from "@base-ui/react/radio";
import { RadioGroup } from "@base-ui/react/radio-group";
import { cn } from "cn";
import { useId } from "react";

export interface Choice<Value extends string> {
  value: Value;
  title: string;
  description: string;
}

export interface ChoiceCardsProps<Value extends string> {
  label: string;
  /** A line under the label about choosing. */
  hint?: string;
  value: Value;
  choices: readonly Choice<Value>[];
  onValueChange: (value: Value) => void;
}

/** Radio options as bordered cards, each with a line on what choosing it means. */
export function ChoiceCards<Value extends string>({
  label,
  hint,
  value,
  choices,
  onValueChange,
}: ChoiceCardsProps<Value>) {
  const labelId = useId();
  const hintId = useId();
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col gap-0.5">
        <span id={labelId} className="font-medium text-ez-text-soft">
          {label}
        </span>
        {hint ? (
          <span id={hintId} className="text-ez-muted">
            {hint}
          </span>
        ) : null}
      </div>
      <RadioGroup
        aria-labelledby={labelId}
        aria-describedby={hint ? hintId : undefined}
        value={value}
        onValueChange={(next) => {
          const choice = choices.find((candidate) => candidate.value === next);
          if (choice) {
            onValueChange(choice.value);
          }
        }}
        className="grid gap-2 sm:grid-cols-2"
      >
        {choices.map((choice) => (
          <ChoiceCard key={choice.value} choice={choice} selected={choice.value === value} />
        ))}
      </RadioGroup>
    </div>
  );
}

function ChoiceCard<Value extends string>({
  choice,
  selected,
}: {
  choice: Choice<Value>;
  selected: boolean;
}) {
  const titleId = useId();
  const descriptionId = useId();
  return (
    <label
      className={cn(
        "flex cursor-pointer gap-3 rounded-md border px-4 py-3",
        selected ? "border-ez-accent bg-ez-surface-muted" : "border-ez-border-strong",
      )}
    >
      <Radio.Root
        value={choice.value}
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        className="mt-0.5 flex size-4 flex-none items-center justify-center rounded-full border border-ez-border-strong bg-ez-surface outline-none focus-visible:ring-3 focus-visible:ring-ez-focus data-checked:border-ez-accent"
      >
        <Radio.Indicator className="size-2 rounded-full bg-ez-accent" />
      </Radio.Root>
      <span className="flex flex-col gap-0.5">
        <span id={titleId} className="font-medium">
          {choice.title}
        </span>
        <span id={descriptionId} className="text-ez-muted">
          {choice.description}
        </span>
      </span>
    </label>
  );
}
