// Compila o módulo nativo e o copia para mate-js/mate.node.
//
// O Cargo gera uma biblioteca dinâmica com nome e extensão que dependem do
// sistema (libmate_js.so, libmate_js.dylib, mate_js.dll). O Node carrega
// qualquer uma delas, desde que o arquivo termine em .node.

"use strict";
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const root = path.join(__dirname, "..", "..");
const release = !process.argv.includes("--debug");

const args = ["build", "-p", "mate-js"];
if (release) args.push("--release");
execFileSync("cargo", args, { cwd: root, stdio: "inherit" });

const names = {
  linux: "libmate_js.so",
  darwin: "libmate_js.dylib",
  win32: "mate_js.dll",
};
const file = names[process.platform];
if (!file) throw new Error(`sistema não suportado: ${process.platform}`);

const from = path.join(root, "target", release ? "release" : "debug", file);
const to = path.join(__dirname, "..", "mate.node");
fs.copyFileSync(from, to);
console.log(`mate.node gerado a partir de ${path.relative(root, from)}`);
