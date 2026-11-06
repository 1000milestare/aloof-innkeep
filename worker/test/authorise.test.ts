import { test } from "node:test";
import assert from "node:assert/strict";
import { authorise, bareAddress, dkimPassesFor } from "../src/index.ts";

const env = { ALLOWED_SENDER: "owner@gmail.com", ALLOWED_DKIM_DOMAIN: "gmail.com" };
const PASS = "mx.cloudflare.net; dkim=pass header.d=gmail.com; spf=pass";

test("bareAddress strips display name and lowercases", () => {
  assert.equal(bareAddress('"Owner" <Owner@Gmail.com>'), "owner@gmail.com");
  assert.equal(bareAddress("owner@gmail.com"), "owner@gmail.com");
});

test("accepts the owner with DKIM pass", () => {
  assert.deepEqual(
    authorise("owner@gmail.com", "Owner <owner@gmail.com>", PASS, env),
    { ok: true },
  );
});

test("refuses another sender", () => {
  const r = authorise("guest@example.com", "guest@example.com", PASS, env);
  assert.equal(r.ok, false);
});

test("refuses a spoofed From header when envelope matches", () => {
  const r = authorise("owner@gmail.com", "boss@example.com", PASS, env);
  assert.equal(r.ok, false);
});

test("refuses when DKIM fails or is for another domain", () => {
  assert.equal(
    authorise("owner@gmail.com", "owner@gmail.com", "mx.cloudflare.net; dkim=fail header.d=gmail.com", env).ok,
    false,
  );
  assert.equal(
    authorise("owner@gmail.com", "owner@gmail.com", "mx.cloudflare.net; dkim=pass header.d=example.com", env).ok,
    false,
  );
});

test("refuses when there is no Authentication-Results header", () => {
  assert.equal(dkimPassesFor(null, "gmail.com"), false);
});
