#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const [input = "/tmp/agentcenter-live-snapshot.txt", output = "site/img/cli-snapshot.svg"] = process.argv.slice(2);

const escapeXml = (value) => value
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;");

const source = fs.readFileSync(input, "utf8").trimEnd().split("\n");
const headerIndex = source.findIndex((line) => line.includes("AGENT") && line.includes("CWD"));
if (headerIndex < 0) throw new Error("Could not find the AgentCenter table header");

const header = source[headerIndex];
const nameStart = header.indexOf("NAME");
const statusStart = header.indexOf("STATUS");
const cwdStart = header.indexOf("CWD");
if ([nameStart, statusStart, cwdStart].some((column) => column < 0)) {
  throw new Error("Expected NAME, STATUS, and CWD columns");
}

const scrubbed = source.map((line, index) => {
  if (index <= headerIndex || line.startsWith("TOTAL ")) return line;
  const scrubbedName = `${line.slice(0, nameStart)}local${" ".repeat(Math.max(1, statusStart - nameStart - 5))}${line.slice(statusStart)}`;
  return `${scrubbedName.slice(0, cwdStart)}~/project`;
});

// A terminal would wrap the long machine summary. Make that wrap explicit so
// the SVG stays readable at README and landing-page widths without dropping data.
const lines = scrubbed.flatMap((line) => {
  const machineStart = line.indexOf("machine:");
  if (!line.startsWith("TOTAL ") || machineStart < 0) return [line];
  return [line.slice(0, machineStart).trimEnd(), `  ${line.slice(machineStart)}`];
});

const width = 1280;
const lineHeight = 33;
const top = 50;
const noteHeight = 44;
const height = top + lines.length * lineHeight + noteHeight;
const text = lines.map((line, index) => {
  const className = index === 0 ? "summary" : index === headerIndex ? "header" : line.startsWith("TOTAL ") ? "total" : "row";
  return `  <text class="${className}" x="36" y="${top + index * lineHeight}">${escapeXml(line)}</text>`;
}).join("\n");

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-labelledby="title desc">
  <title id="title">AgentCenter CLI snapshot</title>
  <desc id="desc">A real release-build snapshot. The session name and working directory have been anonymized.</desc>
  <rect width="${width}" height="${height}" fill="#07100c"/>
  <style>
    text { font: 18px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: #dce8e0; white-space: pre; }
    .summary { fill: #79e2c3; font-weight: 700; }
    .header { fill: #8ea399; font-weight: 700; }
    .total { fill: #ffbe63; }
    .note { fill: #8ea399; font-size: 15px; }
  </style>
${text}
  <line x1="36" y1="${height - 42}" x2="1244" y2="${height - 42}" stroke="#2a3a32"/>
  <text class="note" x="36" y="${height - 15}">Live capture · session name and working directory anonymized</text>
</svg>
`;

fs.mkdirSync(path.dirname(output), { recursive: true });
fs.writeFileSync(output, svg);
console.log(`Wrote ${output} from ${input}`);
