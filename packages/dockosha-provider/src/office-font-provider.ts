import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, extname, join } from "node:path";
import type {
  CanonicalFontRequest,
  FontArtifact,
  FontProvider,
  FontResult,
} from "@samarthmn/doc-yantra-office";
import { z } from "zod";

const requireCjs = createRequire(__filename);

const fontManifestSchema = z.object({
  fonts: z.array(
    z.object({
      file: z.string().regex(/^[A-Za-z0-9._-]+$/),
      family: z.string().min(1),
      version: z.string().min(1),
      sha256: z.string().regex(/^[0-9a-f]{64}$/),
    }),
  ),
});

type SupplementScript = "arabic" | "devanagari" | "hebrew" | "symbols" | "thai";

const SUPPLEMENT_FONTS: Readonly<
  Record<
    SupplementScript,
    {
      file: string;
      coverageScripts: string[];
      fullName: string;
      postscriptName: string;
    }
  >
> = {
  arabic: {
    file: "NotoSansArabic-Variable.ttf",
    coverageScripts: ["arabic"],
    fullName: "Noto Sans Arabic",
    postscriptName: "NotoSansArabic",
  },
  devanagari: {
    file: "NotoSansDevanagari-Variable.ttf",
    coverageScripts: ["devanagari"],
    fullName: "Noto Sans Devanagari",
    postscriptName: "NotoSansDevanagari",
  },
  hebrew: {
    file: "LiberationSans-Regular.ttf",
    coverageScripts: ["hebrew"],
    fullName: "Liberation Sans",
    postscriptName: "LiberationSans",
  },
  symbols: {
    file: "NotoEmoji-Variable.ttf",
    coverageScripts: ["symbols"],
    fullName: "Noto Emoji",
    postscriptName: "NotoEmoji",
  },
  thai: {
    file: "NotoSansThai-Variable.ttf",
    coverageScripts: ["thai"],
    fullName: "Noto Sans Thai",
    postscriptName: "NotoSansThai",
  },
};

const SUPPLEMENT_SCRIPTS = [
  "arabic",
  "devanagari",
  "hebrew",
  "symbols",
  "thai",
] as const satisfies readonly SupplementScript[];

const SUPPLEMENT_SCRIPT_SET: ReadonlySet<string> = new Set(SUPPLEMENT_SCRIPTS);

const isSupplementScript = (value: string): value is SupplementScript =>
  SUPPLEMENT_SCRIPT_SET.has(value);

type CjkVariant = "kr" | "tc" | "jp" | "sc";

const CJK_FONTS: Readonly<
  Record<
    CjkVariant,
    {
      file: string;
      fullName: string;
      postscriptName: string;
      familyHints: readonly string[];
    }
  >
> = {
  kr: {
    file: "NotoSansCJKkr-Regular.otf",
    fullName: "Noto Sans CJK KR",
    postscriptName: "NotoSansCJKkr-Regular",
    familyHints: ["malgun", "batang", "gulim", "dotum", "gungsuh", "kr"],
  },
  tc: {
    file: "NotoSansCJKtc-Regular.otf",
    fullName: "Noto Sans CJK TC",
    postscriptName: "NotoSansCJKtc-Regular",
    familyHints: ["mingliu", "jhenghei", "dfkai", "tc"],
  },
  jp: {
    file: "NotoSansCJKjp-Regular.otf",
    fullName: "Noto Sans CJK JP",
    postscriptName: "NotoSansCJKjp-Regular",
    familyHints: ["mincho", "gothic", "meiryo", "jp"],
  },
  sc: {
    file: "NotoSansCJKsc-Regular.otf",
    fullName: "Noto Sans CJK SC",
    postscriptName: "NotoSansCJKsc-Regular",
    familyHints: [],
  },
};

const CJK_VARIANT_ORDER = ["kr", "tc", "jp", "sc"] as const;
let packagedArtifacts: ReadonlyMap<SupplementScript, FontArtifact> | null =
  null;
const cjkArtifacts = new Map<CjkVariant, FontArtifact>();

const sha256 = (bytes: Uint8Array): string =>
  createHash("sha256").update(bytes).digest("hex");

const fontsDirectory = (): string =>
  realpathSync(
    join(
      dirname(requireCjs.resolve("@samarthmn/doc-yantra/package.json")),
      "fonts",
    ),
  );

const loadManifestArtifact = (
  file: string,
  identity: {
    fullName: string;
    postscriptName: string;
    coverageScripts: string[];
  },
): FontArtifact => {
  const directory = fontsDirectory();
  const manifestValue: unknown = JSON.parse(
    readFileSync(join(directory, "manifest.json"), "utf8"),
  );
  const manifest = fontManifestSchema.parse(manifestValue);
  const entry = manifest.fonts.find((candidate) => candidate.file === file);
  if (!entry) throw new Error(`DocYantra font manifest is missing ${file}`);

  const bytes = new Uint8Array(readFileSync(join(directory, entry.file)));
  const actualHash = sha256(bytes);
  if (actualHash !== entry.sha256) {
    throw new Error(`DocYantra font hash mismatch for ${entry.file}`);
  }
  const extension = extname(entry.file).toLowerCase();
  if (extension !== ".ttf" && extension !== ".otf") {
    throw new Error(`Unsupported packaged font format for ${entry.file}`);
  }

  return {
    family: entry.family,
    fullName: identity.fullName,
    postscriptName: identity.postscriptName,
    weight: 400,
    italic: false,
    stretch: 5,
    version: entry.version,
    faceIndex: 0,
    format: extension === ".otf" ? "otf" : "ttf",
    sha256: actualHash,
    source: `@samarthmn/doc-yantra/fonts/${entry.file}`,
    coverageScripts: identity.coverageScripts,
    license: {
      id: "OFL-1.1",
      embeddable: true,
      subsettable: true,
    },
    bytes,
  };
};

const loadPackagedArtifacts = (): ReadonlyMap<
  SupplementScript,
  FontArtifact
> => {
  if (packagedArtifacts) return packagedArtifacts;
  const loaded = new Map<SupplementScript, FontArtifact>();
  for (const script of SUPPLEMENT_SCRIPTS) {
    loaded.set(
      script,
      loadManifestArtifact(
        SUPPLEMENT_FONTS[script].file,
        SUPPLEMENT_FONTS[script],
      ),
    );
  }
  packagedArtifacts = loaded;
  return loaded;
};

const cjkVariantFor = (request: CanonicalFontRequest): CjkVariant => {
  const families = [request.family, ...request.altFamilies].map((family) =>
    family.toLowerCase(),
  );
  for (const variant of CJK_VARIANT_ORDER) {
    if (
      CJK_FONTS[variant].familyHints.some((hint) =>
        families.some((family) => family.includes(hint)),
      )
    ) {
      return variant;
    }
  }
  return "sc";
};

const cjkArtifactFor = (request: CanonicalFontRequest): FontArtifact => {
  const variant = cjkVariantFor(request);
  const cached = cjkArtifacts.get(variant);
  if (cached) return cached;
  const config = CJK_FONTS[variant];
  const artifact = loadManifestArtifact(config.file, {
    fullName: config.fullName,
    postscriptName: config.postscriptName,
    coverageScripts: ["cjk"],
  });
  cjkArtifacts.set(variant, artifact);
  return artifact;
};

const supplementsFor = (request: CanonicalFontRequest): FontArtifact[] => {
  const artifacts = loadPackagedArtifacts();
  const selected = new Map<string, FontArtifact>();
  for (const script of request.scripts) {
    if (script === "cjk") {
      const artifact = cjkArtifactFor(request);
      selected.set(artifact.sha256, artifact);
      continue;
    }
    if (!isSupplementScript(script)) continue;
    const artifact = artifacts.get(script);
    if (artifact) selected.set(artifact.sha256, artifact);
  }
  return [...selected.values()];
};

const requestKey = (request: CanonicalFontRequest): string =>
  JSON.stringify({
    ...request,
    altFamilies: [...request.altFamilies].sort(),
    scripts: [...request.scripts].sort(),
    slots: [...request.slots].sort(),
    unicodeRanges: [...request.unicodeRanges].sort(
      (left, right) => left - right,
    ),
    vfCoords: [...request.vfCoords].sort(),
  });

const uniqueArtifacts = (artifacts: FontArtifact[]): FontArtifact[] => [
  ...new Map(artifacts.map((artifact) => [artifact.sha256, artifact])).values(),
];

export const createPackagedOfficeFontProvider = (
  primary: FontProvider,
): FontProvider => ({
  id: "dockosha-office-font-provider-v1",
  async cacheKey(request) {
    const primaryKey = primary.cacheKey
      ? await primary.cacheKey(request)
      : "uncached";
    const supplementKey = supplementsFor(request)
      .map((artifact) => artifact.sha256)
      .sort()
      .join(",");
    return `${primary.id}|${primaryKey}|${supplementKey}`;
  },
  async resolve(requests, signal) {
    if (signal.aborted) {
      throw signal.reason ?? new Error("Office font resolution aborted");
    }
    const primaryResults = await primary.resolve(requests, signal);
    const primaryByRequest = new Map(
      primaryResults.map((result) => [requestKey(result.request), result]),
    );

    return requests.map((request): FontResult => {
      const primaryResult = primaryByRequest.get(requestKey(request));
      const supplements = supplementsFor(request);
      if (primaryResult?.status === "found") {
        const artifacts = uniqueArtifacts([
          ...(primaryResult.artifacts ?? [primaryResult.artifact]),
          ...supplements,
        ]);
        return {
          status: "found",
          request,
          artifact: artifacts[0],
          artifacts,
        };
      }
      if (supplements.length > 0) {
        return {
          status: "found",
          request,
          artifact: supplements[0],
          artifacts: supplements,
        };
      }
      return (
        primaryResult ?? {
          status: "temporary_failure",
          request,
          reason: "primary font provider omitted a result",
        }
      );
    });
  },
});
