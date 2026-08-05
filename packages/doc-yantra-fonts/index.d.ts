export type DocYantraFontManifestEntry = Readonly<{
  file: string;
  family: string;
  version: string;
  postscriptName: string;
  byteLength: number;
  format: "ttf" | "otf";
  faceIndex: 0;
  sha256: string;
  license: string;
  coverageClass: string;
}>;

export declare const fontsDirectory: string;
export declare const manifest: Readonly<{
  fonts: readonly DocYantraFontManifestEntry[];
}>;
export declare function resolveFont(file: string): string;
export declare function readFont(file: string): Uint8Array;
