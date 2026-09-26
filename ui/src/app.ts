const API_ROOT = "api/v1/";
const SIGN_IN_POLL_MILLISECONDS = 3000;

interface ErrorBody {
  error?: string;
}

interface AgentStatus {
  agent: string;
  login_prompt: unknown;
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
  const button = form.querySelector("button");
  const idleLabel = button?.textContent ?? "";
  const busyLabel = form.dataset["busy"];
  const body = JSON.stringify(Object.fromEntries(new FormData(form)));
  setError(form, null);
  setBusy(form, true);
  if (button !== null && busyLabel !== undefined) {
    button.textContent = busyLabel;
  }

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

  setBusy(form, false);
  if (button !== null) {
    button.textContent = idleLabel;
  }
}

function setBusy(form: HTMLFormElement, busy: boolean): void {
  form.setAttribute("aria-busy", String(busy));
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

async function reloadWhenSignInEnds(agent: string): Promise<void> {
  try {
    const response = await fetch(API_ROOT + "agents");
    if (!response.ok) {
      return;
    }
    const statuses = (await response.json()) as AgentStatus[];
    const status = statuses.find((candidate) => candidate.agent === agent);
    if (status === undefined || status.login_prompt === null) {
      window.location.reload();
    }
  } catch {
    // The manager may be restarting; try again on the next tick.
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
      // Clipboard access was refused; the code stays on screen to copy by hand.
    },
  );
});
