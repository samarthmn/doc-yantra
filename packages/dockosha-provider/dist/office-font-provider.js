"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.createPackagedOfficeFontProvider = void 0;
const node_crypto_1 = require("node:crypto");
const node_fs_1 = require("node:fs");
const node_module_1 = require("node:module");
const node_path_1 = require("node:path");
const zod_1 = require("zod");
const requireCjs = (0, node_module_1.createRequire)(__filename);
const fontManifestSchema = zod_1.z.object({
    fonts: zod_1.z.array(zod_1.z.object({
        file: zod_1.z.string().regex(/^[A-Za-z0-9._-]+$/),
        family: zod_1.z.string().min(1),
        version: zod_1.z.string().min(1),
        sha256: zod_1.z.string().regex(/^[0-9a-f]{64}$/),
    })),
});
const SUPPLEMENT_FONTS = {
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
];
const SUPPLEMENT_SCRIPT_SET = new Set(SUPPLEMENT_SCRIPTS);
const isSupplementScript = (value) => SUPPLEMENT_SCRIPT_SET.has(value);
const CJK_FONTS = {
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
const CJK_VARIANT_ORDER = ["kr", "tc", "jp", "sc"];
let packagedArtifacts = null;
const cjkArtifacts = new Map();
const sha256 = (bytes) => (0, node_crypto_1.createHash)("sha256").update(bytes).digest("hex");
const fontsDirectory = () => (0, node_fs_1.realpathSync)((0, node_path_1.join)((0, node_path_1.dirname)(requireCjs.resolve("@samarthmn/doc-yantra/package.json")), "fonts"));
const loadManifestArtifact = (file, identity) => {
    const directory = fontsDirectory();
    const manifestValue = JSON.parse((0, node_fs_1.readFileSync)((0, node_path_1.join)(directory, "manifest.json"), "utf8"));
    const manifest = fontManifestSchema.parse(manifestValue);
    const entry = manifest.fonts.find((candidate) => candidate.file === file);
    if (!entry)
        throw new Error(`DocYantra font manifest is missing ${file}`);
    const bytes = new Uint8Array((0, node_fs_1.readFileSync)((0, node_path_1.join)(directory, entry.file)));
    const actualHash = sha256(bytes);
    if (actualHash !== entry.sha256) {
        throw new Error(`DocYantra font hash mismatch for ${entry.file}`);
    }
    const extension = (0, node_path_1.extname)(entry.file).toLowerCase();
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
const loadPackagedArtifacts = () => {
    if (packagedArtifacts)
        return packagedArtifacts;
    const loaded = new Map();
    for (const script of SUPPLEMENT_SCRIPTS) {
        loaded.set(script, loadManifestArtifact(SUPPLEMENT_FONTS[script].file, SUPPLEMENT_FONTS[script]));
    }
    packagedArtifacts = loaded;
    return loaded;
};
const cjkVariantFor = (request) => {
    const families = [request.family, ...request.altFamilies].map((family) => family.toLowerCase());
    for (const variant of CJK_VARIANT_ORDER) {
        if (CJK_FONTS[variant].familyHints.some((hint) => families.some((family) => family.includes(hint)))) {
            return variant;
        }
    }
    return "sc";
};
const cjkArtifactFor = (request) => {
    const variant = cjkVariantFor(request);
    const cached = cjkArtifacts.get(variant);
    if (cached)
        return cached;
    const config = CJK_FONTS[variant];
    const artifact = loadManifestArtifact(config.file, {
        fullName: config.fullName,
        postscriptName: config.postscriptName,
        coverageScripts: ["cjk"],
    });
    cjkArtifacts.set(variant, artifact);
    return artifact;
};
const supplementsFor = (request) => {
    const artifacts = loadPackagedArtifacts();
    const selected = new Map();
    for (const script of request.scripts) {
        if (script === "cjk") {
            const artifact = cjkArtifactFor(request);
            selected.set(artifact.sha256, artifact);
            continue;
        }
        if (!isSupplementScript(script))
            continue;
        const artifact = artifacts.get(script);
        if (artifact)
            selected.set(artifact.sha256, artifact);
    }
    return [...selected.values()];
};
const requestKey = (request) => JSON.stringify({
    ...request,
    altFamilies: [...request.altFamilies].sort(),
    scripts: [...request.scripts].sort(),
    slots: [...request.slots].sort(),
    unicodeRanges: [...request.unicodeRanges].sort((left, right) => left - right),
    vfCoords: [...request.vfCoords].sort(),
});
const uniqueArtifacts = (artifacts) => [
    ...new Map(artifacts.map((artifact) => [artifact.sha256, artifact])).values(),
];
const createPackagedOfficeFontProvider = (primary) => ({
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
        const primaryByRequest = new Map(primaryResults.map((result) => [requestKey(result.request), result]));
        return requests.map((request) => {
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
            return (primaryResult ?? {
                status: "temporary_failure",
                request,
                reason: "primary font provider omitted a result",
            });
        });
    },
});
exports.createPackagedOfficeFontProvider = createPackagedOfficeFontProvider;
//# sourceMappingURL=office-font-provider.js.map