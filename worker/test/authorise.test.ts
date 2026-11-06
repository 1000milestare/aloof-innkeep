import { test } from "node:test";
import assert from "node:assert/strict";
import { bareAddress, classify, dkimPassesFor } from "../src/index.ts";

const env = {
  OWNER_SENDER: "owner@gmail.com",
  OWNER_DKIM_DOMAIN: "gmail.com",
  AIRBNB_SENDER: "automated@airbnb.com",
  AIRBNB_DKIM_DOMAIN: "airbnb.com",
};
const ownerPass = "mx.cloudflare.net; dkim=pass header.d=gmail.com";
const airbnbPass = "mx.cloudflare.net; dkim=pass header.d=airbnb.com";

test("bareAddress strips display name and lowercases", () => {
  assert.equal(bareAddress('"Owner" <Owner@Gmail.com>'), "owner@gmail.com");
});

test("owner mail with DKIM pass is accepted as owner", () => {
  assert.deepEqual(classify("owner@gmail.com", "Owner <owner@gmail.com>", ownerPass, env), { ok: true, kind: "owner" });
});

test("Airbnb automated mail with DKIM pass is accepted as airbnb", () => {
  assert.deepEqual(classify("automated@airbnb.com", "automated@airbnb.com", airbnbPass, env), { ok: true, kind: "airbnb" });
});

test("other senders are refused", () => {
  assert.equal(classify("guest@example.com", "guest@example.com", airbnbPass, env).ok, false);
});

test("express@airbnb.com (guest messages) is refused", () => {
  assert.equal(classify("express@airbnb.com", "express@airbnb.com", airbnbPass, env).ok, false);
});

test("spoofed From header is refused", () => {
  assert.equal(classify("owner@gmail.com", "boss@example.com", ownerPass, env).ok, false);
  assert.equal(classify("automated@airbnb.com", "boss@example.com", airbnbPass, env).ok, false);
});

test("owner or Airbnb mail with the wrong DKIM domain is refused", () => {
  assert.equal(classify("owner@gmail.com", "owner@gmail.com", airbnbPass, env).ok, false);
  assert.equal(classify("automated@airbnb.com", "automated@airbnb.com", ownerPass, env).ok, false);
});

test("missing Authentication-Results is refused", () => {
  assert.equal(dkimPassesFor(null, "gmail.com"), false);
});
