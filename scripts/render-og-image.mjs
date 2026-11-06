#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const output = path.resolve(process.argv[2] || "site/og.png");
const ffmpeg = process.env.AGENTCENTER_FFMPEG || "ffmpeg";
const temp = path.join(os.tmpdir(), `agentcenter-og-${process.pid}.svg`);

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640">
  <defs>
    <symbol id="app-icon" viewBox="0 0 128 128">
      <rect x="12" y="20" width="104" height="96" rx="14" fill="#1a222b"/>
      <rect x="12" y="14" width="104" height="96" rx="14" fill="#2b3642"/>
      <rect x="12" y="14" width="104" height="96" rx="14" fill="none" stroke="#3b4856" stroke-width="2"/>
      <rect x="28" y="32" width="10" height="12" rx="3" fill="#8794a3"/>
      <rect x="44" y="32" width="56" height="12" rx="6" fill="#e6ebf0"/>
      <rect x="28" y="56" width="10" height="12" rx="3" fill="#8794a3"/>
      <rect x="44" y="56" width="40" height="12" rx="6" fill="#f2a81d"/>
      <rect x="28" y="80" width="10" height="12" rx="3" fill="#8794a3"/>
      <rect x="44" y="80" width="24" height="12" rx="6" fill="#e6ebf0"/>
    </symbol>
    <pattern id="grid" width="48" height="48" patternUnits="userSpaceOnUse">
      <path d="M48 0H0V48" fill="none" stroke="#69d9bd" stroke-opacity=".055"/>
    </pattern>
    <radialGradient id="glow" cx="82%" cy="35%" r="45%">
      <stop offset="0" stop-color="#69d9bd" stop-opacity=".22"/>
      <stop offset="1" stop-color="#69d9bd" stop-opacity="0"/>
    </radialGradient>
    <filter id="shadow" x="-40%" y="-40%" width="180%" height="180%">
      <feDropShadow dx="0" dy="28" stdDeviation="32" flood-color="#000000" flood-opacity=".46"/>
    </filter>
  </defs>

  <rect width="1280" height="640" fill="#0b100e"/>
  <rect width="1280" height="640" fill="url(#grid)"/>
  <rect width="1280" height="640" fill="url(#glow)"/>

  <use href="#app-icon" x="82" y="72" width="52" height="52"/>
  <text x="151" y="108" fill="#f1f5f2" font-family="system-ui, sans-serif" font-size="27" font-weight="700">AgentCenter</text>

  <text x="82" y="205" fill="#69d9bd" font-family="monospace" font-size="16" font-weight="700" letter-spacing="2">RESOURCE MONITORING FOR AI CODING SESSIONS</text>
  <text x="82" y="285" fill="#f1f5f2" font-family="system-ui, sans-serif" font-size="67" font-weight="750" letter-spacing="-3">
    <tspan x="82" dy="0">Know what every coding</tspan>
    <tspan x="82" dy="69">agent costs your machine.</tspan>
  </text>

  <g font-family="system-ui, sans-serif" font-size="18" fill="#aebbb2">
    <rect x="82" y="480" width="77" height="42" rx="21" fill="#121a16" stroke="#34443b"/>
    <text x="103" y="507">Linux</text>
    <rect x="173" y="480" width="128" height="42" rx="21" fill="#121a16" stroke="#34443b"/>
    <text x="194" y="507">GTK4 + CLI</text>
    <rect x="315" y="480" width="64" height="42" rx="21" fill="#121a16" stroke="#34443b"/>
    <text x="334" y="507">MIT</text>
  </g>

  <g transform="rotate(3 1070 320)" filter="url(#shadow)">
    <rect x="908" y="124" width="324" height="390" rx="42" fill="#121a16" fill-opacity=".95" stroke="#3d5045"/>
    <use href="#app-icon" x="953" y="184" width="234" height="234"/>
    <text x="1070" y="476" text-anchor="middle" fill="#ffbd63" font-family="monospace" font-size="13" font-weight="700" letter-spacing="2">PRE-ALPHA</text>
  </g>
</svg>
`;

fs.writeFileSync(temp, svg);
fs.mkdirSync(path.dirname(output), { recursive: true });

try {
  const result = spawnSync(ffmpeg, [
    "-y",
    "-loglevel", "error",
    "-i", temp,
    "-frames:v", "1",
    output,
  ], { encoding: "utf8" });

  if (result.status !== 0) {
    process.stderr.write(result.error?.message || result.stderr || result.stdout || "ffmpeg failed without output\n");
    process.exit(result.status || 1);
  }
  console.log(`Wrote ${output} at 1280x640`);
} finally {
  fs.rmSync(temp, { force: true });
}
