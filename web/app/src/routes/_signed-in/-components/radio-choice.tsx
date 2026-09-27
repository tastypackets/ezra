import { useId } from "react";

import {
  Field,
  FieldContent,
  FieldDescription,
  FieldLabel,
  FieldTitle,
} from "@/components/ui/field";
import { RadioGroupItem } from "@/components/ui/radio-group";

export interface RadioChoiceProps {
  value: string;
  title: string;
  description: string;
}

/** One option of a radio group, shown as a card with its title and description. */
export function RadioChoice({ value, title, description }: RadioChoiceProps) {
  const id = useId();
  const titleId = useId();
  const descriptionId = useId();
  return (
    <FieldLabel htmlFor={id}>
      <Field orientation="horizontal">
        <RadioGroupItem
          id={id}
          value={value}
          aria-labelledby={titleId}
          aria-describedby={descriptionId}
        />
        <FieldContent>
          <FieldTitle id={titleId}>{title}</FieldTitle>
          <FieldDescription id={descriptionId}>{description}</FieldDescription>
        </FieldContent>
      </Field>
    </FieldLabel>
  );
}
