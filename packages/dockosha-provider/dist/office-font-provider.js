"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.createPackagedOfficeFontProvider = void 0;
const font_provider_1 = require("@samarthmn/doc-yantra/font-provider");
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
const CJK_REGION_HINTS = {
    kr: [
        "malgun",
        "batang",
        "바탕",
        "gulim",
        "굴림",
        "dotum",
        "돋움",
        "gungsuh",
        "궁서",
        "kr",
    ],
    tc: ["mingliu", "細明體", "新細明體", "jhenghei", "正黑", "dfkai", "標楷", "tc"],
    jp: ["mincho", "明朝", "gothic", "ゴシック", "meiryo", "メイリオ", "yu ", "游", "jp"],
    sc: [],
};
// Windows charset byte (OOXML w:rFonts w:charset / FontSignature) → region.
// 0x81 Hangul, 0x80 Shift-JIS, 0x86 GB2312, 0x88 Big5.
const CJK_CHARSET_REGIONS = {
    "81": "kr",
    "80": "jp",
    "86": "sc",
    "88": "tc",
};
// Word's classic East Asian faces split cleanly into serif (Song/Ming/
// Mincho/Batang lineages) and sans (Hei/Gothic/Malgun/Gulim lineages); Word
// renders them with that style, so the pan-CJK substitute must follow the
// lineage rather than always falling back to Sans. The engine's
// compatible-mode ladder already prefers the Serif face for the serif
// lineages when one is registered.
const CJK_SERIF_HINTS = [
    "simsun",
    "宋体",
    "新宋体",
    "song",
    "mingliu",
    "細明體",
    "新細明體",
    "ming",
    "dfkai",
    "標楷",
    "kaiti",
    "楷",
    "fangsong",
    "仿宋",
    "mincho",
    "明朝",
    "batang",
    "바탕",
    "gungsuh",
    "궁서",
    "serif",
];
const CJK_SANS_HINTS = [
    "yahei",
    "雅黑",
    "jhenghei",
    "正黑",
    "gothic",
    "ゴシック",
    "meiryo",
    "メイリオ",
    "malgun",
    "맑은",
    "gulim",
    "굴림",
    "dotum",
    "돋움",
    "dengxian",
    "等线",
    "simhei",
    "黑体",
    "hei",
    "sans",
];
const CJK_FONTS = {
    sans: {
        kr: {
            file: "NotoSansCJKkr-Regular.otf",
            fullName: "Noto Sans CJK KR",
            postscriptName: "NotoSansCJKkr-Regular",
        },
        tc: {
            file: "NotoSansCJKtc-Regular.otf",
            fullName: "Noto Sans CJK TC",
            postscriptName: "NotoSansCJKtc-Regular",
        },
        jp: {
            file: "NotoSansCJKjp-Regular.otf",
            fullName: "Noto Sans CJK JP",
            postscriptName: "NotoSansCJKjp-Regular",
        },
        sc: {
            file: "NotoSansCJKsc-Regular.otf",
            fullName: "Noto Sans CJK SC",
            postscriptName: "NotoSansCJKsc-Regular",
        },
    },
    serif: {
        kr: {
            file: "NotoSerifCJKkr-Regular.otf",
            fullName: "Noto Serif CJK KR",
            postscriptName: "NotoSerifCJKkr-Regular",
        },
        tc: {
            file: "NotoSerifCJKtc-Regular.otf",
            fullName: "Noto Serif CJK TC",
            postscriptName: "NotoSerifCJKtc-Regular",
        },
        jp: {
            file: "NotoSerifCJKjp-Regular.otf",
            fullName: "Noto Serif CJK JP",
            postscriptName: "NotoSerifCJKjp-Regular",
        },
        sc: {
            file: "NotoSerifCJKsc-Regular.otf",
            fullName: "Noto Serif CJK SC",
            postscriptName: "NotoSerifCJKsc-Regular",
        },
    },
};
const CJK_VARIANT_ORDER = ["kr", "tc", "jp", "sc"];
// Fontsource's Arimo releases stopped carrying Arial's Windows line metrics
// (usWinAscent/Descent 1854/434 became 2136/797 — measured 2026-08-02), which
// inflates every Arial line box ~26% versus Word. Liberation Sans carries the
// true Arial metrics and ships in the optional exact-font pack, so
// Arial-class requests are served by the packaged Liberation Sans faces
// instead of the fetched Arimo artifacts.
const ARIAL_METRIC_FACES = {
    regular: "LiberationSans-Regular.ttf",
    bold: "LiberationSans-Bold.ttf",
    italic: "LiberationSans-Italic.ttf",
    boldItalic: "LiberationSans-BoldItalic.ttf",
};
const MANIFEST_ENTRIES = new Map(font_provider_1.fontManifest.fonts.map((entry) => [entry.file, entry]));
const createArtifactLoader = (provider) => {
    const resolvedFiles = new Map();
    const resolveFile = (file, signal) => {
        const cached = resolvedFiles.get(file);
        if (cached)
            return cached;
        const pending = provider.resolve(file, signal).catch((error) => {
            resolvedFiles.delete(file);
            throw error;
        });
        resolvedFiles.set(file, pending);
        return pending;
    };
    return async (file, identity, signal) => {
        const entry = MANIFEST_ENTRIES.get(file);
        if (!entry)
            throw new Error(`DocYantra font manifest is missing ${file}`);
        const resolution = await resolveFile(file, signal);
        const bytes = (0, font_provider_1.validateFontBytes)(resolution.bytes, entry);
        return {
            family: entry.family,
            fullName: identity.fullName,
            postscriptName: entry.postscriptName,
            weight: identity.weight ?? 400,
            italic: identity.italic ?? false,
            stretch: 5,
            version: entry.version,
            faceIndex: entry.faceIndex,
            format: entry.format,
            sha256: entry.sha256,
            source: resolution.url ??
                `@samarthmn/doc-yantra-fonts:${resolution.source ?? "provider"}/${entry.file}`,
            coverageScripts: identity.coverageScripts,
            license: {
                id: "OFL-1.1",
                embeddable: true,
                subsettable: true,
            },
            bytes,
        };
    };
};
const cjkVariantFor = (request) => {
    const families = [request.family, ...request.altFamilies].map((family) => family.toLowerCase());
    for (const variant of CJK_VARIANT_ORDER) {
        if (CJK_REGION_HINTS[variant].some((hint) => families.some((family) => family.includes(hint)))) {
            return variant;
        }
    }
    const charset = CJK_CHARSET_REGIONS[String(request.charset ?? "").trim()];
    if (charset)
        return charset;
    return "sc";
};
// Style class follows the requested lineage: the primary family decides,
// alternates break ties, and an OS/2 roman family class implies serif.
// Unknown lineages keep the historical Sans default.
const cjkStyleFor = (request) => {
    for (const family of [request.family, ...request.altFamilies]) {
        const lower = family.toLowerCase();
        if (CJK_SERIF_HINTS.some((hint) => lower.includes(hint)))
            return "serif";
        if (CJK_SANS_HINTS.some((hint) => lower.includes(hint)))
            return "sans";
    }
    if (String(request.familyClass ?? "").toLowerCase() === "roman") {
        return "serif";
    }
    return "sans";
};
const cjkConfigFor = (request) => {
    const variant = cjkVariantFor(request);
    const style = cjkStyleFor(request);
    return CJK_FONTS[style][variant];
};
const arialMetricStyle = (request) => {
    const bold = request.weight >= 600;
    if (bold && request.italic)
        return "boldItalic";
    if (bold)
        return "bold";
    if (request.italic)
        return "italic";
    return "regular";
};
const ARIAL_METRIC_IDENTITIES = {
    regular: {
        fullName: "Liberation Sans",
        postscriptName: "LiberationSans",
        weight: 400,
        italic: false,
    },
    bold: {
        fullName: "Liberation Sans Bold",
        postscriptName: "LiberationSans-Bold",
        weight: 700,
        italic: false,
    },
    italic: {
        fullName: "Liberation Sans Italic",
        postscriptName: "LiberationSans-Italic",
        weight: 400,
        italic: true,
    },
    boldItalic: {
        fullName: "Liberation Sans Bold Italic",
        postscriptName: "LiberationSans-BoldItalic",
        weight: 700,
        italic: true,
    },
};
const arialMetricArtifactFor = (request, loadArtifact, signal) => {
    const style = arialMetricStyle(request);
    const identity = ARIAL_METRIC_IDENTITIES[style];
    return loadArtifact(ARIAL_METRIC_FACES[style], {
        ...identity,
        coverageScripts: ["latin"],
    }, signal);
};
// True when the caller literally asked for Arimo (then its own metrics are
// the author's intent and no replacement happens).
const requestsArimoItself = (request) => [request.family, ...request.altFamilies].some((family) => family.trim().toLowerCase() === "arimo");
const supplementConfigurationsFor = (request) => {
    const selected = new Map();
    for (const script of request.scripts) {
        if (script === "cjk") {
            const config = cjkConfigFor(request);
            selected.set(config.file, {
                file: config.file,
                identity: { ...config, coverageScripts: ["cjk"] },
            });
            continue;
        }
        if (!isSupplementScript(script))
            continue;
        const config = SUPPLEMENT_FONTS[script];
        selected.set(config.file, { file: config.file, identity: config });
    }
    return [...selected.values()];
};
const supplementsFor = async (request, loadArtifact, signal) => {
    const selected = new Map();
    for (const { file, identity } of supplementConfigurationsFor(request)) {
        const artifact = await loadArtifact(file, identity, signal);
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
const createPackagedOfficeFontProvider = (primary, options = {}) => {
    const exactProvider = options.fontProvider ?? (0, font_provider_1.createFontProvider)();
    const loadArtifact = createArtifactLoader(exactProvider);
    return {
        id: "dockosha-office-font-provider-v2",
        async cacheKey(request) {
            const primaryKey = primary.cacheKey
                ? await primary.cacheKey(request)
                : "uncached";
            const supplementKey = supplementConfigurationsFor(request)
                .map(({ file }) => MANIFEST_ENTRIES.get(file)?.sha256 ?? file)
                .sort()
                .join(",");
            return `${primary.id}|${primaryKey}|${supplementKey}`;
        },
        async resolve(requests, signal) {
            if (signal.aborted) {
                throw signal.reason ?? new Error("Office font resolution aborted");
            }
            const [primaryResults, supplementsByIndex] = await Promise.all([
                primary.resolve(requests, signal),
                Promise.all(requests.map((request) => supplementsFor(request, loadArtifact, signal))),
            ]);
            const primaryByRequest = new Map(primaryResults.map((result) => [requestKey(result.request), result]));
            return Promise.all(requests.map(async (request, index) => {
                const primaryResult = primaryByRequest.get(requestKey(request));
                const supplements = supplementsByIndex[index];
                if (primaryResult?.status === "found") {
                    let primaryArtifacts = primaryResult.artifacts ?? [primaryResult.artifact];
                    if (!requestsArimoItself(request) &&
                        primaryArtifacts.some((artifact) => artifact.family === "Arimo")) {
                        primaryArtifacts = [
                            await arialMetricArtifactFor(request, loadArtifact, signal),
                            ...primaryArtifacts.filter((artifact) => artifact.family !== "Arimo"),
                        ];
                    }
                    const artifacts = uniqueArtifacts([
                        ...primaryArtifacts,
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
            }));
        },
    };
};
exports.createPackagedOfficeFontProvider = createPackagedOfficeFontProvider;
//# sourceMappingURL=office-font-provider.js.map