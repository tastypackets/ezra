import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";

export interface StateCardProps {
  title: string;
  description: string;
  action: React.ReactNode;
}

/** A small centered card for a page that cannot show its content. */
export function StateCard({ title, description, action }: StateCardProps) {
  return (
    <Card className="mx-auto mt-16 max-w-sm">
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription role="alert">{description}</CardDescription>
      </CardHeader>
      <CardContent>{action}</CardContent>
    </Card>
  );
}
