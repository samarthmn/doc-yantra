import assert from "node:assert/strict";
import { readFileSync, rmSync, mkdtempSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { prepareFontCdnAssets } from "../../scripts/prepare-font-cdn-assets.mjs";
import { createVerifiedCdnFetch } from "../../scripts/verify-font-cdn.mjs";

const ROOT = resolve(fileURLToPath(new URL("../..", import.meta.url)));
test("the CDN bundle carries the R2 policy and font redistribution notices", () => {
  const output = mkdtempSync(join(ROOT, "tmp", "font-cdn-tools-"));
  const originalArgv = process.argv;
  process.argv = [...originalArgv, "--out-dir", output];

  try {
    const result = prepareFontCdnAssets();
    assert.equal(result.output, output);

    const cors = JSON.parse(readFileSync(join(output, "r2-cors.json"), "utf8"));
    assert.deepEqual(cors.rules[0].allowed.origins, ["*"]);
    assert.deepEqual(cors.rules[0].allowed.methods, ["GET", "HEAD"]);

    for (const filename of [
      "LICENSES.md",
      "THIRD_PARTY_NOTICES.md",
      "LICENSE-liberation.txt",
      "LICENSE-noto-cjk.txt",
      "LICENSE-noto-emoji.txt",
      "LICENSE-noto-sans-scripts.txt",
    ]) {
      assert.equal(
        existsSync(join(output, "doc-yantra-fonts", "licenses", filename)),
        true,
        `missing CDN redistribution notice ${filename}`,
      );
    }

    const instructions = readFileSync(join(output, "README.md"), "utf8");
    assert.match(instructions, /Cloudflare R2/u);
    assert.match(instructions, /do not upload `_headers`/iu);
    assert.match(instructions, /font\/ttf/u);
    assert.match(instructions, /font\/otf/u);
  } finally {
    process.argv = originalArgv;
    rmSync(output, { recursive: true, force: true });
  }
});

test("the CDN verifier sends Origin and validates browser-facing font headers", async () => {
  const origins = [];
  const fetchImplementation = async (url, options) => {
    const headers = new Headers(options?.headers);
    origins.push(headers.get("origin"));
    return new Response(new Uint8Array([1, 2, 3]), {
      headers: {
        "Access-Control-Allow-Origin": "*",
        "Cache-Control": "public,max-age=31536000,immutable",
        "Content-Type": "font/ttf",
        "X-Content-Type-Options": "nosniff",
      },
    });
  };

  const verifiedFetch = createVerifiedCdnFetch({
    origin: "https://www.dockosha.com",
    fetchImplementation,
  });
  const response = await verifiedFetch(
    "https://fonts.example.com/doc-yantra-fonts/sha256/hash/Test.ttf",
  );

  assert.equal(response.ok, true);
  assert.deepEqual(new Set(origins), new Set(["https://www.dockosha.com"]));
});

test("the CDN verifier rejects a wrong font MIME type before accepting bytes", async () => {
  const verifiedFetch = createVerifiedCdnFetch({
    origin: "https://www.dockosha.com",
    fetchImplementation: async () =>
      new Response(new Uint8Array(), {
        headers: {
          "Access-Control-Allow-Origin": "*",
          "Cache-Control": "public,max-age=31536000,immutable",
          "Content-Type": "application/octet-stream",
          "X-Content-Type-Options": "nosniff",
        },
      }),
  });
  await assert.rejects(
    verifiedFetch(
      "https://fonts.example.com/doc-yantra-fonts/sha256/hash/Test.ttf",
    ),
    /expected font\/(?:ttf|otf) Content-Type/iu,
  );
});

test("the CDN verifier accepts cache lifetimes longer than one year", async () => {
  const verifiedFetch = createVerifiedCdnFetch({
    origin: "https://www.dockosha.com",
    fetchImplementation: async () =>
      new Response(new Uint8Array(), {
        headers: {
          "Access-Control-Allow-Origin": "*",
          "Cache-Control": "public,max-age=31536001,immutable",
          "Content-Type": "font/otf",
          "X-Content-Type-Options": "nosniff",
        },
      }),
  });

  const response = await verifiedFetch(
    "https://fonts.example.com/doc-yantra-fonts/sha256/hash/Test.otf",
  );
  assert.equal(response.ok, true);
});
