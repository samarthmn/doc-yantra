"use strict";

const { readFileSync } = require("node:fs");
const { join } = require("node:path");

const manifest = require("./fonts/manifest.json");
const fontsDirectory = join(__dirname, "fonts");
const knownFiles = new Set(manifest.fonts.map(({ file }) => file));

function resolveFont(file) {
  if (!knownFiles.has(file)) {
    throw new Error(`unknown DocYantra font: ${file}`);
  }
  return join(fontsDirectory, file);
}

function readFont(file) {
  return new Uint8Array(readFileSync(resolveFont(file)));
}

module.exports = Object.freeze({
  fontsDirectory,
  manifest,
  readFont,
  resolveFont,
});
