import { createHash } from 'node:crypto';
import { lstat, readdir, readFile, readlink, writeFile, mkdir, mkdtemp, realpath, cp, rm } from 'node:fs/promises';
import { dirname, resolve, relative, join, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import ts from 'typescript';
import { version, sourceHashes, transformSources } from './guarded-source.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const targetRoot = resolve(here, '../../target');
const receiptName = 'agentcenter-ready.json';
const capability = 'agentcenter.compactGuarded.v1';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');

// SDK files and generated outputs must be ordinary files, not redirects.
// Dependencies have their own complete inventory and are copied privately.
export async function inventory(root) {
  const files = {};
  async function visit(directory, prefix = '') {
    for (const name of (await readdir(directory)).sort()) {
      if (name === 'node_modules' || (prefix === '' && name === receiptName)) continue;
      const path = join(directory, name), key = prefix ? `${prefix}/${name}` : name;
      const stat = await lstat(path);
      if (stat.isSymbolicLink()) throw new Error(`Symlink in SDK inventory: ${key}`);
      if (stat.isDirectory()) await visit(path, key);
      else if (stat.isFile()) files[key] = { sha256: sha(await readFile(path)), executable: Boolean(stat.mode & 0o111) };
      else throw new Error(`Non-regular SDK input: ${key}`);
    }
  }
  await visit(root);
  const entries = Object.entries(files).sort(([a], [b]) => a.localeCompare(b, 'en'));
  return { files, count: entries.length, digest: sha(JSON.stringify(entries)) };
}

export async function dependencyInventory(root) {
  const stat = await lstat(root);
  if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('Dependency root must be a real directory');
  const base = await realpath(root), files = {};
  async function visit(directory, prefix = '') {
    for (const name of (await readdir(directory)).sort()) {
      const path = join(directory, name), key = prefix ? `${prefix}/${name}` : name;
      const entry = await lstat(path);
      if (entry.isSymbolicLink()) {
        const target = await readlink(path), destination = await realpath(path);
        if (target.startsWith('/') || !destination.startsWith(base + sep)) throw new Error(`External dependency symlink: ${key}`);
        files[key] = { link: target };
      } else if (entry.isDirectory()) await visit(path, key);
      else if (entry.isFile()) files[key] = { sha256: sha(await readFile(path)), executable: Boolean(entry.mode & 0o111) };
      else throw new Error(`Non-regular dependency: ${key}`);
    }
  }
  await visit(base);
  const entries = Object.entries(files).sort(([a], [b]) => a.localeCompare(b, 'en'));
  return { files, count: entries.length, digest: sha(JSON.stringify(entries)) };
}

function once(text, anchor, replacement) {
  if (text.split(anchor).length !== 2) throw new Error('Declaration anchor missing or ambiguous');
  return text.replace(anchor, replacement);
}
function interfaces(source, names) {
  const parsed = ts.createSourceFile('public.ts', source, ts.ScriptTarget.ES2022, true, ts.ScriptKind.TS);
  const printer = ts.createPrinter({ newLine: ts.NewLineKind.LineFeed });
  return names.map(name => {
    const node = parsed.statements.find(s => ts.isInterfaceDeclaration(s) && s.name.text === name);
    if (!node) throw new Error(`Public interface missing: ${name}`);
    return printer.printNode(ts.EmitHint.Unspecified, node, parsed);
  }).join('\n') + '\n';
}
function declarations(name, original, patched) {
  if (name === 'core/session-manager') {
    let text = interfaces(patched[name], ['GuardedCompactionState']) + original;
    text = once(text, '| "getLeafId"', '| "getLeafId" | "getGuardedCompactionState"');
    text = once(text, '    getSessionId(): string;', '    getGuardedCompactionState(): GuardedCompactionState;\n    getSessionId(): string;');
    const parsed = ts.createSourceFile('manager.ts', patched[name], ts.ScriptTarget.ES2022, true, ts.ScriptKind.TS);
    const manager = parsed.statements.find(s => ts.isClassDeclaration(s) && s.name?.text === 'SessionManager');
    const method = manager?.members.find(m => ts.isMethodDeclaration(m) && m.name.getText(parsed) === 'appendGuardedCompaction');
    if (!method) throw new Error('Guarded append declaration missing');
    const declaration = ts.factory.updateMethodDeclaration(method, method.modifiers, method.asteriskToken,
      method.name, method.questionToken, method.typeParameters, method.parameters, method.type, undefined);
    const signature = ts.createPrinter().printNode(ts.EmitHint.Unspecified, declaration, parsed);
    return once(text, '    appendCompaction<T = unknown>(', `    ${signature}\n    appendCompaction<T = unknown>(`);
  }
  if (name === 'core/extensions/types') {
    let text = 'import type { GuardedCompactionState } from "../session-manager.ts";\n' +
      interfaces(patched[name], ['GuardedCompactOptions', 'GuardedCompactMetadata']) + original;
    text = once(text, '    compact(options?: CompactOptions): void;', '    compact(options?: CompactOptions): void;\n    readonly compactGuardedCapability?: "agentcenter.compactGuarded.v1";\n    compactGuarded?(options: GuardedCompactOptions): Promise<GuardedCompactMetadata>;');
    return once(text, '    compact: (options?: CompactOptions) => void;', '    compact: (options?: CompactOptions) => void;\n    compactGuarded?: (options: GuardedCompactOptions) => Promise<GuardedCompactMetadata>;');
  }
  if (name === 'core/agent-session') return 'import type { GuardedCompactOptions, GuardedCompactMetadata } from "./extensions/types.ts";\n' +
    once(original, '    compact(customInstructions?: string): Promise<CompactionResult>;', '    compact(customInstructions?: string): Promise<CompactionResult>;\n    readonly compactGuardedCapability: "agentcenter.compactGuarded.v1";\n    compactGuarded(options: GuardedCompactOptions): Promise<GuardedCompactMetadata>;');
  if (name === 'index' || name === 'core/extensions/index') return once(original, 'CompactOptions,', 'CompactOptions, GuardedCompactOptions, GuardedCompactMetadata,') +
    `\nexport type { GuardedCompactionState } from "${name === 'index' ? './core' : '..'}/session-manager.ts";\n`;
  return original;
}

async function outputPath(path) {
  const output = resolve(path);
  if (!output.startsWith(targetRoot + sep)) throw new Error('Private runtime must be inside this checkout target/');
  const target = await lstat(targetRoot);
  if (!target.isDirectory() || target.isSymbolicLink() || target.uid !== process.getuid()) throw new Error('Unsafe target directory');
  const parent = await realpath(dirname(output));
  if (!parent.startsWith((await realpath(targetRoot)) + sep) && parent !== await realpath(targetRoot)) throw new Error('Redirected output parent');
  if (parent !== dirname(output)) throw new Error('Symlink in output parent');
  return output;
}
async function readManifest() {
  return JSON.parse(await readFile(join(here, 'manifest.json'), 'utf8'));
}

export async function verifyPrivateRuntime(path) {
  const output = await outputPath(path);
  const stat = await lstat(output);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o700) throw new Error('Unsafe private runtime');
  const receiptFile = join(output, receiptName), receiptStat = await lstat(receiptFile);
  if (!receiptStat.isFile() || receiptStat.isSymbolicLink() || receiptStat.uid !== process.getuid()) throw new Error('Unsafe runtime receipt');
  const receipt = JSON.parse(await readFile(receiptFile, 'utf8'));
  const manifest = await readManifest();
  if (receipt.format !== 1 || receipt.capability !== capability || receipt.sourceDigest !== manifest.sdk_inventory_sha256 ||
      receipt.compiler !== manifest.compiler || receipt.transformer !== sha(await readFile(join(here, 'guarded-source.mjs')))) throw new Error('Incompatible runtime receipt');
  if ((await inventory(output)).digest !== receipt.outputDigest) throw new Error('Private runtime was modified or is incomplete');
  if ((await dependencyInventory(join(output, 'node_modules'))).digest !== manifest.dependencies_inventory_sha256)
    throw new Error('Private dependency inventory mismatch');
  return receipt;
}

export async function preparePrivateRuntime(sourcePath, destination) {
  const source = await realpath(sourcePath), output = await outputPath(destination);
  if (source === output || source.startsWith(output + sep) || output.startsWith(source + sep)) throw new Error('Source/output overlap');
  try { await lstat(output); throw new Error('Output already exists'); } catch (e) { if (e.code !== 'ENOENT') throw e; }
  const manifest = await readManifest();
  const pkg = JSON.parse(await readFile(join(source, 'package.json'), 'utf8'));
  if (pkg.name !== manifest.package || pkg.version !== version || manifest.version !== version || ts.version !== manifest.compiler) throw new Error('Unsupported SDK/compiler version');
  const before = await inventory(source);
  if (before.digest !== manifest.sdk_inventory_sha256 || before.count !== manifest.sdk_inventory_files) throw new Error('Pinned SDK inventory mismatch');
  const dependencies = await dependencyInventory(join(source, 'node_modules'));
  if (dependencies.digest !== manifest.dependencies_inventory_sha256 || dependencies.count !== manifest.dependencies_inventory_files)
    throw new Error('Pinned dependency inventory mismatch');
  const sources = {};
  for (const name of Object.keys(sourceHashes)) {
    const map = JSON.parse(await readFile(join(source, 'dist', `${name}.js.map`), 'utf8'));
    if (map.sourcesContent?.length !== 1 || typeof map.sourcesContent[0] !== 'string') throw new Error('Missing pinned source map');
    sources[name] = map.sourcesContent[0];
  }
  const patched = transformSources(pkg.version, sources);
  // Compile all patches and declarations before creating a private output.
  const changes = new Map();
  for (const [name, text] of Object.entries(patched)) {
    const compiled = ts.transpileModule(text, { fileName: `${name}.ts`, reportDiagnostics: true, compilerOptions: {
      target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler,
      rewriteRelativeImportExtensions: true, verbatimModuleSyntax: true, sourceMap: true, inlineSources: true,
    } });
    if (compiled.diagnostics?.some(d => d.category === ts.DiagnosticCategory.Error)) throw new Error('TypeScript syntax transform failed');
    changes.set(`dist/${name}.js`, compiled.outputText);
    changes.set(`dist/${name}.js.map`, compiled.sourceMapText);
    const declaration = await readFile(join(source, 'dist', `${name}.d.ts`), 'utf8');
    changes.set(`dist/${name}.d.ts`, declarations(name, declaration, patched));
  }
  const stage = await mkdtemp(join(dirname(output), '.pi-guarded-stage-'));
  try {
    for (const [name, expected] of Object.entries(before.files)) {
      const bytes = await readFile(join(source, name));
      if (sha(bytes) !== expected.sha256) throw new Error('SDK changed during copy');
      const dest = join(stage, name);
      await mkdir(dirname(dest), { recursive: true, mode: 0o700 });
      await writeFile(dest, bytes, { mode: expected.executable ? 0o700 : 0o600, flag: 'wx' });
    }
    for (const [name, bytes] of changes) await writeFile(join(stage, name), bytes);
    await writeFile(join(stage, 'LICENSE.agentcenter-pi'), await readFile(join(here, 'LICENSE.pi')), { mode: 0o600, flag: 'wx' });
    await writeFile(join(stage, 'NOTICE.agentcenter'), await readFile(join(here, 'NOTICE')), { mode: 0o600, flag: 'wx' });
    await cp(join(source, 'node_modules'), join(stage, 'node_modules'), { recursive: true, dereference: false, verbatimSymlinks: true, errorOnExist: true, force: false });
    if ((await dependencyInventory(join(stage, 'node_modules'))).digest !== dependencies.digest)
      throw new Error('Dependencies changed during private copy');
    const launch = `import { spawn } from 'node:child_process';\nimport { dirname, join } from 'node:path';\nimport { fileURLToPath } from 'node:url';\nimport { verifyPrivateRuntime } from ${JSON.stringify(import.meta.url)};\nconst root = dirname(fileURLToPath(import.meta.url));\nawait verifyPrivateRuntime(root);\nconst child = spawn(process.execPath, [join(root, 'dist/cli.js'), ...process.argv.slice(2)], { stdio: 'inherit', env: { ...process.env, PI_PACKAGE_DIR: root } });\nfor (const signal of ['SIGINT','SIGTERM']) process.on(signal, () => child.kill(signal));\nchild.on('error', error => { console.error(error.message); process.exitCode = 1; });\nchild.on('exit', (code, signal) => { process.exitCode = code ?? (signal === 'SIGINT' ? 130 : 1); });\n`;
    await writeFile(join(stage, 'run.mjs'), launch, { mode: 0o600, flag: 'wx' });
    if ((await inventory(source)).digest !== before.digest ||
        (await dependencyInventory(join(source, 'node_modules'))).digest !== dependencies.digest)
      throw new Error('SDK/dependencies changed while preparing runtime');
    const receipt = { format: 1, capability, source, sourceDigest: before.digest, compiler: ts.version,
      transformer: sha(await readFile(join(here, 'guarded-source.mjs'))), outputDigest: (await inventory(stage)).digest };
    await writeFile(join(stage, receiptName), JSON.stringify(receipt, null, 2) + '\n', { mode: 0o600, flag: 'wx' });
    // Linux coreutils no-clobber rename: never replace an existing output.
    const moved = spawnSync('/usr/bin/mv', ['-T', '-n', '--', stage, output], { encoding: 'utf8' });
    if (moved.status !== 0) throw new Error('Unable to publish private runtime');
    try { await lstat(stage); throw new Error('Output already exists; private stage not published'); } catch (e) { if (e.code !== 'ENOENT') throw e; }
    await verifyPrivateRuntime(output);
    return { output, receipt };
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [command, source, output] = process.argv.slice(2);
  if (command === 'inventory' && source && !output) console.log(JSON.stringify(await inventory(await realpath(source)), null, 2));
  else if (command === 'prepare' && source && output) console.log(JSON.stringify(await preparePrivateRuntime(source, output), null, 2));
  else if (command === 'verify' && source && !output) console.log(JSON.stringify(await verifyPrivateRuntime(source), null, 2));
  else { console.error('Usage: node prepare.mjs inventory SDK | prepare SDK CHECKOUT_TARGET_OUTPUT | verify CHECKOUT_TARGET_OUTPUT'); process.exitCode = 2; }
}
