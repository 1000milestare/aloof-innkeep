import { test } from "node:test";
import assert from "node:assert/strict";
import { bareAddress, classify, dkimAligned, parseList } from "../src/index.ts";

const AIRBNB = "automated@airbnb.com";
const OWNERS = parseList("owner1@gmail.com, Owner2@Example.com ,owner3@example.org");
const airbnbPass = "mx.cloudflare.net; dkim=pass header.i=@email.airbnb.com header.s=s20150428";
const gmailPass = "mx.cloudflare.net; dkim=pass header.d=gmail.com";

test("bareAddress strips display name and lowercases", () => {
  assert.equal(bareAddress('"Owner" <Owner@Gmail.com>'), "owner@gmail.com");
});

test("parseList splits, trims, lowercases and drops junk", () => {
  assert.deepEqual(parseList(" A@x.com, ,b@y.com "), ["a@x.com", "b@y.com"]);
  assert.deepEqual(parseList(undefined), []);
});

test("each listed owner is accepted with aligned DKIM", () => {
  assert.deepEqual(classify("owner1@gmail.com", gmailPass, OWNERS, AIRBNB), { ok: true, kind: "owner" });
  assert.deepEqual(classify("Owner2 <owner2@example.com>", "dkim=pass header.d=example.com", OWNERS, AIRBNB), { ok: true, kind: "owner" });
  assert.deepEqual(classify("owner3@example.org", "dkim=pass header.d=example.org", OWNERS, AIRBNB), { ok: true, kind: "owner" });
});

test("Airbnb automated mail is accepted when DKIM is signed by a subdomain", () => {
  assert.deepEqual(classify("Airbnb <automated@airbnb.com>", airbnbPass, OWNERS, AIRBNB), { ok: true, kind: "airbnb" });
});

test("unlisted senders are not acted on", () => {
  assert.equal(classify("guest@example.com", "dkim=pass header.d=example.com", OWNERS, AIRBNB).ok, false);
});

test("guest messages from express@airbnb.com are not acted on", () => {
  assert.equal(classify("express@airbnb.com", airbnbPass, OWNERS, AIRBNB).ok, false);
});

test("a listed address with no DKIM pass is not acted on", () => {
  assert.equal(classify("owner1@gmail.com", null, OWNERS, AIRBNB).ok, false);
  assert.equal(classify("owner1@gmail.com", "dkim=fail header.d=gmail.com", OWNERS, AIRBNB).ok, false);
});

test("DKIM for a different domain does not vouch for the From domain", () => {
  assert.equal(classify("owner1@gmail.com", "dkim=pass header.d=example.com", OWNERS, AIRBNB).ok, false);
  assert.equal(classify("automated@airbnb.com", gmailPass, OWNERS, AIRBNB).ok, false);
});

test("subdomain alignment works in both directions, but not for lookalikes", () => {
  assert.equal(dkimAligned("dkim=pass header.d=mail.example.com", "example.com"), true);
  assert.equal(dkimAligned("dkim=pass header.d=notexample.com", "example.com"), false);
  assert.equal(dkimAligned("dkim=pass header.d=example.com.evil.net", "example.com"), false);
});
