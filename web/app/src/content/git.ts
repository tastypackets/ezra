/** Names the variables gh could take a token from, such as "GH_TOKEN or GITHUB_TOKEN". */
export const eitherVariable = (variables: string[]) => variables.join(" or ");

export const GIT_DESCRIPTIONS = {
  title: "Git",
  description: "Agents push to GitHub over HTTPS with this sign-in and commit with this identity.",
  github: (host: string) => (host === "github.com" ? "GitHub" : host),
  signed_in_as: (account: string) => `Signed in as ${account}`,
  signed_in: "Signed in",
  signed_out: "Signed out",
  signing_in: "Signing in",
  not_confirmed: "Not confirmed",
  from_environment: (variables: string[]) => `Set by the ${eitherVariable(variables)} variable.`,
  sign_in: "Sign in to GitHub",
  sign_out: "Sign out",
  start_over: "Start over",
  identity: "Commit identity",
  name: "Name",
  email: "Email",
  save: "Save",
  saved: "Commit identity saved.",
} as const;
