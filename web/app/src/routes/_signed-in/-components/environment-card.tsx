import type { EnvironmentSettings } from "@ezra/client";
import { useSuspenseQuery } from "@tanstack/react-query";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ENVIRONMENT_DESCRIPTIONS } from "@/content/manager";
import { managerQueryOptions } from "@/queries/manager-queries";

/** The container settings that come from its variables, which the page cannot change. */
export function EnvironmentCard() {
  const { data: manager } = useSuspenseQuery(managerQueryOptions);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{ENVIRONMENT_DESCRIPTIONS.title}</CardTitle>
        <CardDescription>{ENVIRONMENT_DESCRIPTIONS.description}</CardDescription>
      </CardHeader>
      <CardContent>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-3">
          {environmentRows(manager.environment).map((row) => (
            <div key={row.label} className="contents">
              <dt className="flex flex-col">
                <span className="text-muted-foreground">{row.label}</span>
                <span className="font-mono text-xs text-muted-foreground">{row.source}</span>
              </dt>
              <dd className="break-words">{row.value}</dd>
            </div>
          ))}
        </dl>
      </CardContent>
    </Card>
  );
}

function listOrNone(values: string[]) {
  return values.length > 0 ? values.join(", ") : ENVIRONMENT_DESCRIPTIONS.none;
}

function onOrOff(on: boolean) {
  return on ? ENVIRONMENT_DESCRIPTIONS.on : ENVIRONMENT_DESCRIPTIONS.off;
}

function environmentRows(environment: EnvironmentSettings) {
  return [
    { label: ENVIRONMENT_DESCRIPTIONS.hostname, source: "hostname", value: environment.hostname },
    { label: ENVIRONMENT_DESCRIPTIONS.port, source: "EZRA_PORT", value: String(environment.port) },
    {
      label: ENVIRONMENT_DESCRIPTIONS.tls_verification,
      source: "EZRA_TLS_VERIFY",
      value: onOrOff(environment.tls_verification),
    },
    { label: ENVIRONMENT_DESCRIPTIONS.sudo, source: "EZRA_SUDO", value: onOrOff(environment.sudo) },
    {
      label: ENVIRONMENT_DESCRIPTIONS.apt_packages,
      source: "EZRA_APT_PACKAGES",
      value: listOrNone(environment.apt_packages),
    },
    {
      label: ENVIRONMENT_DESCRIPTIONS.setup_scripts,
      source: "/etc/ezra/setup.d",
      value: listOrNone(environment.setup_scripts),
    },
    {
      label: ENVIRONMENT_DESCRIPTIONS.github_token,
      source: "GH_TOKEN or GITHUB_TOKEN",
      value: environment.github_token
        ? ENVIRONMENT_DESCRIPTIONS.set
        : ENVIRONMENT_DESCRIPTIONS.not_set,
    },
  ];
}
