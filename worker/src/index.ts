// Cloudflare Email Worker. Every message is forwarded to a human copy. Only mail
// from a trusted sender, whose DKIM signature is aligned with the From domain, is
// stored in KV and starts the GitHub Actions sync. The Rust sync does all parsing.
//
// Nothing here bounces mail: a message we don't act on is still delivered.

export interface Env {
  MAIL: KVNamespace; // KV binding (wrangler.toml)
  OWNER_SENDERS: string; // secret: comma-separated owner addresses
  FORWARD_TO: string; // secret: verified destination for the human copy
  AIRBNB_SENDER: string; // "automated@airbnb.com"
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

/** Comma-separated list of addresses, normalised. Empty entries are dropped. */
export function parseList(value: string | undefined): string[] {
  return (value ?? "")
    .split(",")
    .map((s) => bareAddress(s))
    .filter((s) => s.includes("@"));
}

/**
 * True if some DKIM pass in Cloudflare's Authentication-Results is signed by the
 * sender's domain or a subdomain of it (Airbnb signs as email.airbnb.com).
 * Only the first Authentication-Results header is read; it is added by Cloudflare.
 */
export function dkimAligned(authResults: string | null, senderDomain: string): boolean {
  if (!authResults) return false;
  const domain = senderDomain.toLowerCase();
  const pattern = /dkim=pass[^;]*?header\.[di]=@?([a-z0-9.-]+)/g;
  for (const match of authResults.toLowerCase().matchAll(pattern)) {
    const signer = match[1];
    if (signer === domain || signer.endsWith(`.${domain}`)) return true;
  }
  return false;
}

export type Kind = "owner" | "airbnb";

/**
 * Identify a trusted sender from the From header. The envelope sender is not used:
 * for Airbnb it is a bounce address, not the From address.
 */
export function classify(
  headerFrom: string | null,
  authResults: string | null,
  owners: string[],
  airbnbSender: string,
): { ok: true; kind: Kind } | { ok: false; reason: string } {
  const sender = headerFrom ? bareAddress(headerFrom) : "";
  const domain = sender.split("@")[1] ?? "";
  if (!sender || !domain) return { ok: false, reason: "no usable From header" };

  let kind: Kind;
  if (sender === airbnbSender.toLowerCase()) kind = "airbnb";
  else if (owners.includes(sender)) kind = "owner";
  else return { ok: false, reason: `sender not on the list: ${sender}` };

  if (!dkimAligned(authResults, domain)) {
    return { ok: false, reason: `DKIM not aligned with ${domain}` };
  }
  return { ok: true, kind };
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
      message.headers.get("from"),
      message.headers.get("authentication-results"), // first one only: added by Cloudflare
      parseList(env.OWNER_SENDERS),
      env.AIRBNB_SENDER,
    );

    // Store and dispatch only for trusted mail. Errors are logged, never thrown.
    let stored = false;
    if (check.ok) {
      try {
        if (message.rawSize > MAX_RAW_BYTES) {
          console.warn("Trusted mail too large to store; not acted on");
        } else {
          const raw = await new Response(message.raw).text();
          const key = `mail:${Date.now()}:${crypto.randomUUID()}`;
          await env.MAIL.put(key, raw, { expirationTtl: RETENTION_SECONDS });
          stored = true;
        }
      } catch (err) {
        console.error(`Store failed: ${err}`);
      }
    } else {
      console.log(`Not acted on (still forwarded): ${check.reason}`);
    }

    // Human copy, for every sender.
    try {
      await message.forward(env.FORWARD_TO);
    } catch (err) {
      console.error(`Forward failed: ${err}`);
    }

    if (stored) {
      ctx.waitUntil(
        dispatchWorkflow(env)
          .then(async (res) => {
            if (!res.ok) console.error(`Dispatch failed: ${res.status} ${await res.text()}`);
          })
          .catch((err) => console.error(`Dispatch error: ${err}`)),
      );
    }
  },
};
