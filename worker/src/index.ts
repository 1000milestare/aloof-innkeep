// Cloudflare Email Worker: a minimal relay. For mail from the owner or from
// Airbnb's automated sender, it stores the raw message in KV and starts the
// GitHub Actions sync. It does not parse mail; the Rust sync does that.

export interface Env {
  MAIL: KVNamespace; // KV binding (wrangler.toml)
  OWNER_SENDER: string; // owner's address, lowercase
  OWNER_DKIM_DOMAIN: string; // e.g. "gmail.com"
  AIRBNB_SENDER: string; // "automated@airbnb.com"
  AIRBNB_DKIM_DOMAIN: string; // "airbnb.com"
  GITHUB_REPO: string;
  GITHUB_WORKFLOW: string;
  GITHUB_REF: string;
  GITHUB_TOKEN: string; // secret: fine-grained, Actions: write on the repo
}

const RETENTION_SECONDS = 60 * 60 * 24 * 120;
const MAX_RAW_BYTES = 1024 * 1024;

/** Lowercased bare address from `"Name" <a@b.com>` or `a@b.com`. */
export function bareAddress(value: string): string {
  const match = value.match(/<([^>]+)>/);
  return (match ? match[1] : value).trim().toLowerCase();
}

/** True only if the first Authentication-Results header (added by Cloudflare) shows DKIM pass for the domain. */
export function dkimPassesFor(authResults: string | null, domain: string): boolean {
  if (!authResults) return false;
  const lower = authResults.toLowerCase();
  return lower.includes("dkim=pass") && lower.includes(`header.d=${domain.toLowerCase()}`);
}

export type Kind = "owner" | "airbnb";

/** Classify a message as owner mail, Airbnb automated mail, or neither (refused). */
export function classify(
  envelopeFrom: string,
  headerFrom: string | null,
  firstAuthResults: string | null,
  env: Pick<Env, "OWNER_SENDER" | "OWNER_DKIM_DOMAIN" | "AIRBNB_SENDER" | "AIRBNB_DKIM_DOMAIN">,
): { ok: true; kind: Kind } | { ok: false; reason: string } {
  const envelope = bareAddress(envelopeFrom);
  const header = headerFrom ? bareAddress(headerFrom) : "";

  if (envelope === env.OWNER_SENDER.toLowerCase() && header === envelope) {
    if (dkimPassesFor(firstAuthResults, env.OWNER_DKIM_DOMAIN)) return { ok: true, kind: "owner" };
    return { ok: false, reason: "owner DKIM did not pass" };
  }
  if (envelope === env.AIRBNB_SENDER.toLowerCase() && header === envelope) {
    if (dkimPassesFor(firstAuthResults, env.AIRBNB_DKIM_DOMAIN)) return { ok: true, kind: "airbnb" };
    return { ok: false, reason: "Airbnb DKIM did not pass" };
  }
  return { ok: false, reason: "sender not recognised" };
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
      "User-Agent": "aloof-innkeep-mail-relay",
    },
    body: JSON.stringify({ ref: env.GITHUB_REF }),
  });
}

export default {
  async email(message: ForwardableEmailMessage, env: Env, ctx: ExecutionContext): Promise<void> {
    const check = classify(
      message.from,
      message.headers.get("from"),
      message.headers.get("authentication-results"), // first one only: added by Cloudflare
      env,
    );
    if (!check.ok) {
      console.warn(`Refused mail: ${check.reason}`);
      message.setReject("Not accepted");
      return;
    }
    if (message.rawSize > MAX_RAW_BYTES) {
      message.setReject("Message too large");
      return;
    }

    const raw = await new Response(message.raw).text();
    const key = `mail:${Date.now()}:${crypto.randomUUID()}`;
    await env.MAIL.put(key, raw, { expirationTtl: RETENTION_SECONDS });

    ctx.waitUntil(
      dispatchWorkflow(env).then(async (res) => {
        if (!res.ok) console.error(`Workflow dispatch failed: ${res.status} ${await res.text()}`);
      }),
    );
  },
};
