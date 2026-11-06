// Cloudflare Email Worker: turns a command email from the owner into a run of
// the GitHub Actions sync. The Worker does no parsing of the command itself;
// it only (1) checks the sender, (2) forwards the mail to Gmail so the sync can
// read it, and (3) starts the workflow. The Rust sync is the single source of
// truth for what a command means.

export interface Env {
  // Plain vars (wrangler.toml)
  ALLOWED_SENDER: string; // e.g. "owner@gmail.com" (lowercase)
  ALLOWED_DKIM_DOMAIN: string; // e.g. "gmail.com"
  FORWARD_TO: string; // verified Cloudflare destination, e.g. the Gmail inbox
  GITHUB_REPO: string; // "owner/repo"
  GITHUB_WORKFLOW: string; // e.g. "run.yml"
  GITHUB_REF: string; // e.g. "main"
  // Secret (wrangler secret put GITHUB_TOKEN): fine-grained, Actions:write on the repo
  GITHUB_TOKEN: string;
}

/** Lowercased bare address from a header value such as `"Name" <a@b.com>`. */
export function bareAddress(value: string): string {
  const match = value.match(/<([^>]+)>/);
  return (match ? match[1] : value).trim().toLowerCase();
}

/**
 * True only when the first Authentication-Results header from Cloudflare shows a
 * DKIM pass for the allowed domain. Later Authentication-Results headers come
 * from the sender and can't be trusted, so only the first one is read.
 */
export function dkimPassesFor(authResults: string | null, domain: string): boolean {
  if (!authResults) return false;
  const lower = authResults.toLowerCase();
  return (
    lower.includes("dkim=pass") &&
    lower.includes(`header.d=${domain.toLowerCase()}`)
  );
}

/** Decide whether a message may trigger a run. Returns a reason when refused. */
export function authorise(
  envelopeFrom: string,
  headerFrom: string | null,
  firstAuthResults: string | null,
  env: Pick<Env, "ALLOWED_SENDER" | "ALLOWED_DKIM_DOMAIN">,
): { ok: true } | { ok: false; reason: string } {
  const allowed = env.ALLOWED_SENDER.toLowerCase();
  if (bareAddress(envelopeFrom) !== allowed) {
    return { ok: false, reason: "envelope sender not allowed" };
  }
  if (!headerFrom || bareAddress(headerFrom) !== allowed) {
    return { ok: false, reason: "From header not allowed" };
  }
  if (!dkimPassesFor(firstAuthResults, env.ALLOWED_DKIM_DOMAIN)) {
    return { ok: false, reason: "DKIM did not pass for the allowed domain" };
  }
  return { ok: true };
}

async function dispatchWorkflow(env: Env): Promise<Response> {
  const url = `https://api.github.com/repos/${env.GITHUB_REPO}/actions/workflows/${env.GITHUB_WORKFLOW}/dispatches`;
  return fetch(url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.GITHUB_TOKEN}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "Content-Type": "application/json",
      "User-Agent": "aloof-innkeep-command-worker",
    },
    body: JSON.stringify({ ref: env.GITHUB_REF }),
  });
}

export default {
  async email(
    message: ForwardableEmailMessage,
    env: Env,
    ctx: ExecutionContext,
  ): Promise<void> {
    const check = authorise(
      message.from,
      message.headers.get("from"),
      // Only the first Authentication-Results header is trusted (added by Cloudflare).
      message.headers.get("authentication-results"),
      env,
    );
    if (!check.ok) {
      console.warn(`Refused command mail: ${check.reason}`);
      message.setReject("Not authorised");
      return;
    }

    // Forward first so Gmail keeps the record the sync reads from.
    await message.forward(env.FORWARD_TO);

    // Start a run in the background; the email response doesn't wait for GitHub.
    ctx.waitUntil(
      dispatchWorkflow(env).then(async (res) => {
        if (!res.ok) {
          console.error(`Workflow dispatch failed: ${res.status} ${await res.text()}`);
        } else {
          console.log("Workflow dispatched");
        }
      }),
    );
  },
};
