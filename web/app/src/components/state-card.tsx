import { Card } from "@/components/ui/card";

export interface StateCardProps {
  title: string;
  description: string;
  action: React.ReactNode;
}

/** A small centered card for a page that cannot show its content. */
export function StateCard({ title, description, action }: StateCardProps) {
  return (
    <Card className="mx-auto mt-16 max-w-sm p-5">
      <h2 className="text-base font-semibold">{title}</h2>
      <p role="alert" className="mt-0.5 text-ez-muted">
        {description}
      </p>
      <div className="mt-4">{action}</div>
    </Card>
  );
}
