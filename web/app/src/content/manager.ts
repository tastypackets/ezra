export const MANAGER_DESCRIPTIONS = {
  title: "Manager",
  password: "Password",
  current_password: "Current password",
  new_password: "New password",
  change_password: "Change password",
  password_changed: "Password changed, other sessions signed out.",
  sessions: "Sessions",
  other_sessions: (count: number) =>
    count === 0
      ? "No other sessions are signed in."
      : count === 1
        ? "1 other session is signed in."
        : `${count} other sessions are signed in.`,
  end_other_sessions: "Sign out other sessions",
  other_sessions_ended: "Other sessions signed out.",
  certificate: "Certificate",
  self_signed: "Self-signed",
  hostnames: "Hostnames",
  expires: "Expires",
  fingerprint: "SHA-256 fingerprint",
  copy_fingerprint: "Copy",
  copied_fingerprint: "Copied",
  not_covered: (hostname: string) => `Does not cover ${hostname}`,
  no_certificate: "This manager serves no certificate of its own.",
  regenerate: "Regenerate",
  regenerate_title: "Regenerate the certificate?",
  regenerate_description: "Browsers warn again until you accept the new certificate.",
  regenerate_confirm: "Regenerate certificate",
  regenerated: "Certificate regenerated.",
  cancel: "Cancel",
} as const;

export const ENVIRONMENT_DESCRIPTIONS = {
  title: "Environment",
  description: "Read from the container's variables at start.",
  hostname: "Hostname",
  port: "Port",
  tls_verification: "Check download certificates",
  sudo: "Sudo for agents",
  apt_packages: "Extra apt packages",
  setup_scripts: "Setup scripts",
  github_token: "GitHub token",
  on: "On",
  off: "Off",
  set: "Set",
  not_set: "Not set",
  none: "None",
} as const;
