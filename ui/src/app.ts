const API_ROOT = "api/v1/";
const SIGN_IN_POLL_MILLISECONDS = 3000;
const INSTALL_POLL_MILLISECONDS = 500;

interface ErrorBody {
  error?: string;
}

interface DownloadProgress {
  received_bytes: number;
  total_bytes: number | null;
}

interface AgentStatus {
  agent: string;
  login_prompt: unknown;
  install_progress: DownloadProgress | null;
}

document.addEventListener("submit", (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement)) {
    return;
  }
  const path = form.dataset["api"];
  if (path === undefined) {
    return;
  }
  event.preventDefault();
  void submitToApi(form, path);
});

async function submitToApi(form: HTMLFormElement, path: string): Promise<void> {
  const label = form.querySelector<HTMLElement>(".button-label");
  const idleLabel = label?.textContent ?? "";
  const body = JSON.stringify(Object.fromEntries(new FormData(form)));
  setError(form, null);
  setBusy(form, true);
  const progressAgent = form.dataset["progressFor"];
  const progressTimer =
    progressAgent !== undefined && label !== null
      ? window.setInterval(() => void showInstallProgress(progressAgent, label), INSTALL_POLL_MILLISECONDS)
      : undefined;

  try {
    const response = await fetch(API_ROOT + path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body,
    });
    if (response.ok) {
      window.location.reload();
      return;
    }
    setError(form, await errorMessageOf(response));
  } catch {
    setError(form, "Could not reach the manager.");
  }

  window.clearInterval(progressTimer);
  setBusy(form, false);
  if (label !== null) {
    label.textContent = idleLabel;
  }
}

function setBusy(form: HTMLFormElement, busy: boolean): void {
  form.setAttribute("aria-busy", String(busy));
  form.querySelector("button")?.setAttribute("aria-busy", String(busy));
  for (const control of form.querySelectorAll("button, input")) {
    if (control instanceof HTMLButtonElement || control instanceof HTMLInputElement) {
      control.disabled = busy;
    }
  }
}

function setError(form: HTMLFormElement, message: string | null): void {
  const errorElement = form.querySelector<HTMLElement>("[data-error]");
  if (errorElement === null) {
    return;
  }
  errorElement.textContent = message ?? "";
  errorElement.hidden = message === null;
}

async function errorMessageOf(response: Response): Promise<string> {
  const text = await response.text().catch(() => "");
  try {
    const body = JSON.parse(text) as ErrorBody;
    if (typeof body.error === "string" && body.error !== "") {
      return body.error;
    }
  } catch {
    // Not JSON, e.g. a request the server could not parse.
  }
  return text.trim() !== "" ? text.trim() : `Request failed (${response.status})`;
}

async function fetchAgentStatus(agent: string): Promise<AgentStatus | undefined> {
  const response = await fetch(API_ROOT + "agents");
  if (!response.ok) {
    throw new Error(`agent status request failed with ${response.status}`);
  }
  const statuses = (await response.json()) as AgentStatus[];
  return statuses.find((candidate) => candidate.agent === agent);
}

function percentOf(progress: DownloadProgress): number | undefined {
  if (progress.total_bytes === null || progress.total_bytes === 0) {
    return undefined;
  }
  return Math.min(100, Math.floor((progress.received_bytes * 100) / progress.total_bytes));
}

async function showInstallProgress(agent: string, label: HTMLElement): Promise<void> {
  try {
    const progress = (await fetchAgentStatus(agent))?.install_progress;
    const percent = progress === null || progress === undefined ? undefined : percentOf(progress);
    if (percent !== undefined) {
      label.textContent = `${percent}%`;
    }
  } catch {
    // Progress is optional. The install request reports success or failure.
  }
}

async function reloadWhenInstallEnds(agent: string, label: HTMLElement): Promise<void> {
  try {
    const progress = (await fetchAgentStatus(agent))?.install_progress;
    if (progress === null || progress === undefined) {
      window.location.reload();
      return;
    }
    const percent = percentOf(progress);
    if (percent !== undefined) {
      label.textContent = `${percent}%`;
    }
  } catch {
    // The manager may be restarting. The next tick tries again.
  }
}

async function reloadWhenSignInEnds(agent: string): Promise<void> {
  try {
    const status = await fetchAgentStatus(agent);
    if (status === undefined || status.login_prompt === null) {
      window.location.reload();
    }
  } catch {
    // The manager may be restarting. The next tick tries again.
  }
}

for (const running of document.querySelectorAll<HTMLElement>("[data-install-running]")) {
  const agent = running.dataset["installRunning"];
  const label = running.querySelector<HTMLElement>(".button-label");
  if (agent !== undefined && label !== null) {
    window.setInterval(() => void reloadWhenInstallEnds(agent, label), INSTALL_POLL_MILLISECONDS);
  }
}

for (const waiting of document.querySelectorAll<HTMLElement>("[data-waiting-for]")) {
  const agent = waiting.dataset["waitingFor"];
  if (agent !== undefined) {
    window.setInterval(() => void reloadWhenSignInEnds(agent), SIGN_IN_POLL_MILLISECONDS);
  }
}

const COPIED_LABEL_MILLISECONDS = 1500;

document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element)) {
    return;
  }
  const button = event.target.closest<HTMLButtonElement>("button[data-copy]");
  const text = button?.dataset["copy"];
  if (button === null || text === undefined) {
    return;
  }
  const idleLabel = button.textContent;
  navigator.clipboard.writeText(text).then(
    () => {
      button.textContent = "Copied";
      window.setTimeout(() => {
        button.textContent = idleLabel;
      }, COPIED_LABEL_MILLISECONDS);
    },
    () => {
      // Clipboard access was refused. The code stays on screen to copy by hand.
    },
  );
});
