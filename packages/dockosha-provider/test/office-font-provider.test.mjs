import assert from "node:assert/strict";
import test from "node:test";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const offlineFonts = require("../../doc-yantra-fonts");

const { createPackagedOfficeFontProvider } = await import(
  "../dist/office-font-provider.js"
);

const request = (overrides = {}) => ({
  family: "Batang",
  altFamilies: [],
  weight: 400,
  italic: false,
  stretch: 5,
  slots: ["ascii", "east_asia"],
  panose: null,
  familyClass: null,
  pitch: null,
  charset: null,
  unicodeRanges: [],
  scripts: ["cjk", "common"],
  vfCoords: [],
  ...overrides,
});

const notFoundPrimary = {
  id: "stub-not-found",
  resolve: async (requests) =>
    requests.map((req) => ({ status: "not_found", request: req })),
};

const resolveOne = async (provider, req) => {
  const signal = new AbortController().signal;
  const [result] = await provider.resolve([req], signal);
  return result;
};

const exactProvider = (calls = []) => ({
  async resolve(file) {
    calls.push(file);
    return { bytes: offlineFonts.readFont(file), source: "offline", durationMs: 0 };
  },
});

const packagedProvider = (primary, calls) =>
  createPackagedOfficeFontProvider(primary, { fontProvider: exactProvider(calls) });

const winMetrics = (bytes) => {
  const buf = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const numTables = buf.readUInt16BE(4);
  let os2 = 0;
  let head = 0;
  for (let i = 0; i < numTables; i += 1) {
    const off = 12 + i * 16;
    const tag = buf.toString("ascii", off, off + 4);
    if (tag === "OS/2") os2 = buf.readUInt32BE(off + 8);
    if (tag === "head") head = buf.readUInt32BE(off + 8);
  }
  return {
    upem: buf.readUInt16BE(head + 18),
    winAsc: buf.readUInt16BE(os2 + 74),
    winDesc: buf.readUInt16BE(os2 + 76),
  };
};

test("serif CJK lineages resolve to the packaged Noto Serif regional face", async () => {
  const provider = packagedProvider(notFoundPrimary);
  const cases = [
    [request({ family: "Batang", familyClass: "roman" }), "Noto Serif CJK KR"],
    [request({ family: "SimSun" }), "Noto Serif CJK SC"],
    [request({ family: "MS Mincho" }), "Noto Serif CJK JP"],
    [request({ family: "MingLiU" }), "Noto Serif CJK TC"],
  ];
  for (const [req, expected] of cases) {
    const result = await resolveOne(provider, req);
    assert.equal(result.status, "found", `${req.family} must resolve`);
    assert.equal(result.artifact.fullName, expected, req.family);
  }
});

test("sans/gothic CJK lineages keep the Noto Sans regional face", async () => {
  const provider = packagedProvider(notFoundPrimary);
  const cases = [
    [request({ family: "Malgun Gothic" }), "Noto Sans CJK KR"],
    [request({ family: "Microsoft YaHei" }), "Noto Sans CJK SC"],
    [request({ family: "MS Gothic" }), "Noto Sans CJK JP"],
    [request({ family: "Microsoft JhengHei" }), "Noto Sans CJK TC"],
  ];
  for (const [req, expected] of cases) {
    const result = await resolveOne(provider, req);
    assert.equal(result.artifact.fullName, expected, req.family);
  }
});

test("unknown CJK families pick the region from the Windows charset", async () => {
  const provider = packagedProvider(notFoundPrimary);
  const cases = [
    ["81", "Noto Sans CJK KR"],
    ["80", "Noto Sans CJK JP"],
    ["86", "Noto Sans CJK SC"],
    ["88", "Noto Sans CJK TC"],
  ];
  for (const [charset, expected] of cases) {
    const result = await resolveOne(
      provider,
      request({ family: "Mystery UI", charset }),
    );
    assert.equal(result.artifact.fullName, expected, `charset ${charset}`);
  }
});

test("Arial-class Arimo artifacts are replaced by Liberation Sans with Arial's Windows metrics", async () => {
  const fakeArimo = (weight) => ({
    family: "Arimo",
    fullName: `Arimo ${weight}`,
    postscriptName: `Arimo-${weight}`,
    weight,
    italic: false,
    stretch: 5,
    version: "v-test",
    faceIndex: 0,
    format: "ttf",
    sha256: `${weight}`.repeat(32).slice(0, 64).padEnd(64, "0"),
    source: "stub",
    coverageScripts: ["latin"],
    license: { id: "OFL-1.1", embeddable: true, subsettable: true },
    bytes: new Uint8Array([0, 1, 0, 0, 0, 0]),
  });
  const primary = {
    id: "stub-arimo",
    resolve: async (requests) =>
      requests.map((req) => ({
        status: "found",
        request: req,
        artifact: fakeArimo(req.weight),
        artifacts: [fakeArimo(req.weight)],
      })),
  };
  const provider = packagedProvider(primary);

  const regular = await resolveOne(
    provider,
    request({ family: "Arial", scripts: ["latin"], slots: ["ascii"] }),
  );
  assert.equal(regular.artifact.family, "Liberation Sans");
  assert.equal(regular.artifact.weight, 400);
  const vitals = winMetrics(regular.artifact.bytes);
  assert.deepEqual(
    vitals,
    { upem: 2048, winAsc: 1854, winDesc: 434 },
    "replacement must carry Arial's Windows line metrics",
  );

  const bold = await resolveOne(
    provider,
    request({ family: "Arial", weight: 700, scripts: ["latin"], slots: ["ascii"] }),
  );
  assert.equal(bold.artifact.fullName, "Liberation Sans Bold");
  assert.equal(bold.artifact.weight, 700);

  const literalArimo = await resolveOne(
    provider,
    request({ family: "Arimo", scripts: ["latin"], slots: ["ascii"] }),
  );
  assert.equal(
    literalArimo.artifact.family,
    "Arimo",
    "a literal Arimo request keeps the author's chosen face",
  );
});

test("loads only requested packaged faces and reuses validated results", async () => {
  const calls = [];
  const provider = packagedProvider(notFoundPrimary, calls);
  const batang = request({ family: "Batang", familyClass: "roman" });

  await resolveOne(provider, batang);
  await resolveOne(provider, batang);

  assert.deepEqual(calls, ["NotoSerifCJKkr-Regular.otf"]);
});
