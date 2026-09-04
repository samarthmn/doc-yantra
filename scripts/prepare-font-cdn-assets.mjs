#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TMP_ROOT = join(ROOT, "tmp");
const PACKAGE_ROOT = join(ROOT, "packages", "doc-yantra-fonts");
const FONT_ROOT = join(PACKAGE_ROOT, "fonts");

function outputDirectory(version) {
  const at = process.argv.indexOf("--out-dir");
  const output = resolve(ROOT, at === -1 ? `tmp/font-cdn-assets-${version}` : process.argv[at + 1]);
  const relativePath = relative(TMP_ROOT, output);
  if (!relativePath || relativePath === ".." || relativePath.startsWith(`..${sep}`)) {
    throw new Error(`font CDN output must be a child of ${TMP_ROOT}`);
  }
  if (existsSync(output) && readdirSync(output).length > 0) {
    throw new Error(`font CDN output directory is not empty: ${output}`);
  }
  return output;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export function prepareFontCdnAssets() {
  const metadata = JSON.parse(
    readFileSync(join(PACKAGE_ROOT, "package.json"), "utf8"),
  );
  const manifest = JSON.parse(
    readFileSync(join(FONT_ROOT, "manifest.json"), "utf8"),
  );
  const output = outputDirectory(metadata.version);
  const publicRoot = join(output, "doc-yantra-fonts");
  const checksums = [];
  for (const entry of manifest.fonts) {
    const source = join(FONT_ROOT, entry.file);
    const bytes = readFileSync(source);
    if (bytes.byteLength !== entry.byteLength || sha256(bytes) !== entry.sha256) {
      throw new Error(`${entry.file}: canonical file differs from its manifest`);
    }
    const relativePath = join("sha256", entry.sha256, entry.file);
    const destination = join(publicRoot, relativePath);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(source, destination);
    checksums.push(`${entry.sha256}  doc-yantra-fonts/${relativePath}`);
  }
  writeFileSync(join(output, "SHA256SUMS"), `${checksums.join("\n")}\n`);
  writeFileSync(
    join(publicRoot, "manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );

  const licensesRoot = join(publicRoot, "licenses");
  mkdirSync(licensesRoot, { recursive: true });
  for (const filename of ["LICENSES.md", "THIRD_PARTY_NOTICES.md"]) {
    copyFileSync(join(PACKAGE_ROOT, filename), join(licensesRoot, filename));
  }
  for (const filename of readdirSync(FONT_ROOT)
    .filter((entry) => entry.startsWith("LICENSE-") && entry.endsWith(".txt"))
    .sort()) {
    copyFileSync(join(FONT_ROOT, filename), join(licensesRoot, filename));
  }

  const r2CorsPolicy = {
    rules: [
      {
        allowed: {
          origins: ["*"],
          methods: ["GET", "HEAD"],
          headers: ["*"],
        },
        exposeHeaders: ["ETag"],
        maxAgeSeconds: 86_400,
      },
    ],
  };
  writeFileSync(
    join(output, "r2-cors.json"),
    `${JSON.stringify(r2CorsPolicy, null, 2)}\n`,
  );
  writeFileSync(
    join(output, "_headers"),
    "/doc-yantra-fonts/sha256/*\n  Access-Control-Allow-Origin: *\n  Cache-Control: public, max-age=31536000, immutable\n  X-Content-Type-Options: nosniff\n",
  );
  writeFileSync(
    join(output, "README.md"),
    [
      "# Cloudflare R2 font deployment bundle",
      "",
      "Upload `doc-yantra-fonts/` to the root of the R2 bucket whose active custom domain is `public.dockosha.com`.",
      "Apply `r2-cors.json` to that bucket before verification. Cloudflare R2 does not consume static-host `_headers` files, so do not upload `_headers`.",
      "",
      "Use `font/ttf` for `.ttf` objects and `font/otf` for `.otf` objects. Set `Cache-Control: public,max-age=31536000,immutable` on everything under `doc-yantra-fonts/sha256/`.",
      "Upload `manifest.json` as `application/json` and the files under `licenses/` as text with short or no caching.",
      "Never use `--delete`; the bucket may contain unrelated public assets.",
      "",
      "Example commands (replace the placeholders and use a short-lived, bucket-scoped AWS profile):",
      "",
      "```bash",
      "npx wrangler r2 bucket cors set <BUCKET_NAME> --file r2-cors.json",
      "aws s3 cp doc-yantra-fonts/sha256/ s3://<BUCKET_NAME>/doc-yantra-fonts/sha256/ --recursive --exclude '*' --include '*.ttf' --content-type font/ttf --cache-control 'public,max-age=31536000,immutable' --endpoint-url https://<ACCOUNT_ID>.r2.cloudflarestorage.com --profile dockosha-fonts-r2",
      "aws s3 cp doc-yantra-fonts/sha256/ s3://<BUCKET_NAME>/doc-yantra-fonts/sha256/ --recursive --exclude '*' --include '*.otf' --content-type font/otf --cache-control 'public,max-age=31536000,immutable' --endpoint-url https://<ACCOUNT_ID>.r2.cloudflarestorage.com --profile dockosha-fonts-r2",
      "aws s3 cp doc-yantra-fonts/manifest.json s3://<BUCKET_NAME>/doc-yantra-fonts/manifest.json --content-type application/json --cache-control 'no-cache' --endpoint-url https://<ACCOUNT_ID>.r2.cloudflarestorage.com --profile dockosha-fonts-r2",
      "aws s3 cp doc-yantra-fonts/licenses/ s3://<BUCKET_NAME>/doc-yantra-fonts/licenses/ --recursive --content-type text/plain --cache-control 'no-cache' --endpoint-url https://<ACCOUNT_ID>.r2.cloudflarestorage.com --profile dockosha-fonts-r2",
      "```",
      "",
      "Then add the documented Cloudflare response-header rule for `X-Content-Type-Options: nosniff` and run `scripts/verify-font-cdn.mjs` with both `--base-url` and `--origin`.",
      "",
    ].join("\n"),
  );
  return Object.freeze({
    output,
    version: metadata.version,
    fontCount: manifest.fonts.length,
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const result = prepareFontCdnAssets();
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
