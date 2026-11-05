#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const [input = "-", output = "site/img/cli-snapshot.svg"] = process.argv.slice(2);

const escapeXml = (value) => value
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;");

const sourceText = input === "-" ? fs.readFileSync(0, "utf8") : fs.readFileSync(input, "utf8");
const source = sourceText.trimEnd().split("\n");
const headerIndex = source.findIndex((line) => line.includes("AGENT") && line.includes("CWD"));
if (headerIndex < 0) throw new Error("Could not find the AgentCenter table header");

const headers = source[headerIndex].trim().split(/\s+/);
const expectedHeaders = [
  "AGENT", "PID", "PROCS", "CPU%", "PSS", "SWAP", "READ/s", "WRITE/s",
  "STALL", "FLAG", "NAME", "STATUS", "CTX", "CWD",
];
if (headers.join("\0") !== expectedHeaders.join("\0")) {
  throw new Error("Unexpected AgentCenter table columns");
}

const totalIndex = source.findIndex((line, index) => index > headerIndex && line.startsWith("TOTAL "));
if (totalIndex < 0) throw new Error("Could not find the AgentCenter total row");

const parseSession = (line) => {
  const fields = line.trim().split(/\s+/);
  if (fields.length < headers.length) throw new Error(`Unexpected session row: ${line}`);
  return [...fields.slice(0, headers.length - 1), fields.slice(headers.length - 1).join(" ")];
};

const sessions = source.slice(headerIndex + 1, totalIndex).map(parseSession);

// Privacy scrub, applied uniformly to every session row: replace the NAME cell
// with "local" and the complete CWD cell with "~/project". Agent ids, PIDs,
// states, counts, and resource measurements stay exactly as captured.
const scrubbedSessions = sessions.map((fields) => {
  const scrubbed = [...fields];
  scrubbed[10] = "local";
  scrubbed[13] = "~/project";
  return scrubbed;
});

const renderGroup = (indices) => {
  const widths = indices.map((index) => Math.max(
    headers[index].length,
    ...scrubbedSessions.map((fields) => fields[index].length),
  ));
  const render = (fields) => indices
    .map((index, position) => fields[index].padEnd(widths[position]))
    .join("  ")
    .trimEnd();
  return [
    { className: "header", value: render(headers) },
    ...scrubbedSessions.map((fields) => ({ className: "row", value: render(fields) })),
  ];
};

const [totalMetrics, machineSource] = source[totalIndex].split(/\s+machine:\s+/, 2);
const total = totalMetrics.trim().split(/\s+/);
if (total.length !== 11 || total[0] !== "TOTAL") {
  throw new Error("Unexpected AgentCenter total row");
}

const totalLines = [
  `TOTAL ${total[1]}  PID ${total[2]}  PROCS ${total[3]}  CPU% ${total[4]}  PSS ${total[5]}  SWAP ${total[6]}`,
  `         READ/s ${total[7]}  WRITE/s ${total[8]}  STALL ${total[9]}  FLAG ${total[10]}`,
].map((value) => ({ className: "total", value }));

// The raw machine summary also contains instantaneous CPU and PSI readings.
// Omit those fields; retain their captured values nowhere. If present, the SVG
// machine line contains only the original core count, memory, and swap values.
const machineMatch = machineSource?.match(
  /^(\d+ cores), [^,]+ cpu, mem ([^,]+), swap ([^,]+), psi /,
);
const machineLine = machineMatch
  ? [{ className: "machine", value: `machine: ${machineMatch[1]}, mem ${machineMatch[2]}, swap ${machineMatch[3]}` }]
  : [];

// Split the wide table into metric and identity groups. This changes only the
// layout, not any retained value, and keeps the capture readable on a phone.
const lines = [
  { className: "summary", value: source[0] },
  ...renderGroup([0, 1, 2, 3, 4, 5, 6, 7, 8]),
  ...renderGroup([9, 10, 11, 12, 13]),
  ...totalLines,
  ...machineLine,
];

const width = 960;
const lineHeight = 34;
const top = 48;
const noteHeight = 48;
const height = top + lines.length * lineHeight + noteHeight;
const text = lines.map((line, index) => (
  `  <text class="${line.className}" x="32" y="${top + index * lineHeight}">${escapeXml(line.value)}</text>`
)).join("\n");

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-labelledby="title desc">
  <title id="title">AgentCenter CLI snapshot</title>
  <desc id="desc">Output from a real release-build run, reformatted for narrow screens. Session display names and working directories are replaced. PIDs and displayed measurements are unchanged. Machine CPU and PSI fields are omitted.</desc>
  <rect width="${width}" height="${height}" fill="#07100c"/>
  <style>
    text { font: 20px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: #dce8e0; white-space: pre; }
    .summary { fill: #79e2c3; font-weight: 700; }
    .header { fill: #8ea399; font-weight: 700; }
    .total { fill: #ffbe63; }
    .machine { fill: #a8c5b6; }
    .note { fill: #8ea399; font-size: 14px; }
  </style>
${text}
  <line x1="32" y1="${height - 44}" x2="${width - 32}" y2="${height - 44}" stroke="#2a3a32"/>
  <text class="note" x="32" y="${height - 16}">Real release run · display NAME/CWD replaced · PIDs/values unchanged · machine CPU/PSI omitted</text>
</svg>
`;

fs.mkdirSync(path.dirname(output), { recursive: true });
fs.writeFileSync(output, svg);
console.log(`Wrote ${output} from ${input === "-" ? "stdin" : input}; captured PID(s): ${sessions.map((fields) => fields[1]).join(", ")}`);
