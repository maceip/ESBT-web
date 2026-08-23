#!/usr/bin/env node

import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const abiPath = resolve(root, 'abi/esbt-wasm-v1.json');
const definition = await readFile(abiPath, 'utf8');
const abi = JSON.parse(definition);

validateDefinition(abi);

const options = parseArguments(process.argv.slice(2));
if (options.outputs.length === 0 && (!options.verifyWasm || options.check)) {
  options.outputs.push({ format: 'javascript', path: resolve(root, 'web/esbt-abi.generated.js') });
}

for (const output of options.outputs) {
  const rendered = render(output.format);
  if (options.check) {
    const current = await readFile(output.path, 'utf8').catch(() => null);
    if (current !== rendered) {
      throw new Error(`generated ABI binding is stale: ${output.path}`);
    }
  } else {
    await mkdir(dirname(output.path), { recursive: true });
    await writeFile(output.path, rendered);
    console.log(`wrote ${output.path}`);
  }
}

if (options.verifyWasm) {
  const bytes = await readFile(options.verifyWasm);
  const module = new WebAssembly.Module(bytes);
  const instance = new WebAssembly.Instance(module, {});
  verifyModule(module, instance.exports);
  console.log(`verified ESBT Wasm ABI v${abi.version}: ${options.verifyWasm}`);
}

function parseArguments(argv) {
  const parsed = { check: false, outputs: [], verifyWasm: null };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--check') {
      parsed.check = true;
      continue;
    }
    if (argument === '--typescript' || argument === '--javascript' || argument === '--verify-wasm') {
      const value = argv[index + 1];
      if (!value || value.startsWith('--')) throw new Error(`${argument} requires a path`);
      index += 1;
      if (argument === '--verify-wasm') parsed.verifyWasm = resolve(value);
      else parsed.outputs.push({ format: argument.slice(2), path: resolve(value) });
      continue;
    }
    throw new Error(`unknown argument: ${argument}`);
  }
  return parsed;
}

function validateDefinition(value) {
  if (value?.schema !== 'esbt.wasm-abi' || value.version !== 1) {
    throw new Error('unsupported ESBT Wasm ABI definition');
  }
  if (!/^[a-z][a-z0-9_.-]*$/.test(value.custom_section) || value.memory !== 'memory') {
    throw new Error('invalid ABI memory or custom-section name');
  }
  if (!Array.isArray(value.imports) || !Array.isArray(value.functions)) {
    throw new Error('invalid ABI collections');
  }
  const names = new Set();
  for (const fn of value.functions) {
    if (!/^esbt_[a-z0-9_]+$/.test(fn.name) || names.has(fn.name)) {
      throw new Error(`invalid or duplicate ABI function: ${fn.name}`);
    }
    names.add(fn.name);
    if (!Array.isArray(fn.parameters) || !['i32', 'u32', 'pointer', 'void'].includes(fn.result)) {
      throw new Error(`invalid ABI signature: ${fn.name}`);
    }
    const parameters = new Set();
    for (const parameter of fn.parameters) {
      if (
        !/^[a-zA-Z][a-zA-Z0-9]*$/.test(parameter.name) ||
        parameters.has(parameter.name) ||
        !['i32', 'u32', 'pointer'].includes(parameter.type)
      ) {
        throw new Error(`invalid ABI parameter in ${fn.name}`);
      }
      parameters.add(parameter.name);
    }
  }
}

function render(format) {
  const typescript = format === 'typescript';
  const banner = '// Generated from abi/esbt-wasm-v1.json by tools/wasm-abi.mjs. Do not edit.\n';
  const typeSurface = typescript
    ? `\nexport interface EsbtExports {\n  memory: WebAssembly.Memory;\n${abi.functions
        .map((fn) => {
          const parameters = fn.parameters.map((parameter) => `${parameter.name}: number`).join(', ');
          const result = fn.result === 'void' ? 'void' : 'number';
          return `  ${fn.name}(${parameters}): ${result};`;
        })
        .join('\n')}\n}\n`
    : '';
  const signature = typescript
    ? `(module: WebAssembly.Module, rawExports: WebAssembly.Exports): EsbtExports`
    : `(module, rawExports)`;
  const cast = typescript ? ' as unknown as EsbtExports' : '';

  return `${banner}
export const ESBT_ABI_VERSION = ${abi.version};
export const ESBT_ABI_CUSTOM_SECTION = ${JSON.stringify(abi.custom_section)};
export const ESBT_ABI_DEFINITION = ${JSON.stringify(definition)};
export const ESBT_ABI_FUNCTIONS = Object.freeze(${JSON.stringify(
    abi.functions.map((fn) => ({ name: fn.name, arity: fn.parameters.length })),
    null,
    2,
  )});
${typeSurface}
export function checkedEsbtExports${signature} {
  const fail = (detail${typescript ? ': string' : ''})${typescript ? ': never' : ''} => {
    throw new TypeError(\`esbt: Wasm ABI mismatch (\${detail})\`);
  };
  const sections = WebAssembly.Module.customSections(module, ESBT_ABI_CUSTOM_SECTION);
  if (sections.length !== 1) fail('missing embedded ABI definition');
  let embedded;
  try {
    embedded = new TextDecoder('utf-8', { fatal: true }).decode(sections[0]);
  } catch {
    fail('embedded ABI definition is not UTF-8');
  }
  if (embedded !== ESBT_ABI_DEFINITION) fail('binding and artifact definitions differ');

  const imports = WebAssembly.Module.imports(module);
  if (imports.length !== 0) fail('artifact unexpectedly imports host capabilities');
  const descriptors = new Map(
    WebAssembly.Module.exports(module).map((descriptor) => [descriptor.name, descriptor.kind]),
  );
  if (descriptors.get(${JSON.stringify(abi.memory)}) !== 'memory') fail('memory export is absent');
  if (!(rawExports[${JSON.stringify(abi.memory)}] instanceof WebAssembly.Memory)) {
    fail('memory export has the wrong runtime type');
  }

  const declared = new Set(ESBT_ABI_FUNCTIONS.map((fn) => fn.name));
  for (const descriptor of WebAssembly.Module.exports(module)) {
    if (descriptor.name.startsWith('esbt_') && !declared.has(descriptor.name)) {
      fail(\`undeclared engine export \${descriptor.name}\`);
    }
  }
  for (const expected of ESBT_ABI_FUNCTIONS) {
    if (descriptors.get(expected.name) !== 'function') fail(\`missing function \${expected.name}\`);
    const value = rawExports[expected.name];
    if (typeof value !== 'function' || value.length !== expected.arity) {
      fail(\`wrong arity for \${expected.name}\`);
    }
  }
  return rawExports${cast};
}
`;
}

function verifyModule(module, exports) {
  const sections = WebAssembly.Module.customSections(module, abi.custom_section);
  if (sections.length !== 1 || new TextDecoder('utf-8', { fatal: true }).decode(sections[0]) !== definition) {
    throw new Error('artifact does not embed the source ABI definition');
  }
  if (WebAssembly.Module.imports(module).length !== abi.imports.length) {
    throw new Error('artifact import surface differs from ABI definition');
  }
  const descriptors = new Map(
    WebAssembly.Module.exports(module).map((descriptor) => [descriptor.name, descriptor.kind]),
  );
  if (descriptors.get(abi.memory) !== 'memory' || !(exports[abi.memory] instanceof WebAssembly.Memory)) {
    throw new Error('artifact memory export differs from ABI definition');
  }
  const declared = new Set(abi.functions.map((fn) => fn.name));
  for (const [name] of descriptors) {
    if (name.startsWith('esbt_') && !declared.has(name)) {
      throw new Error(`artifact has undeclared engine export ${name}`);
    }
  }
  for (const fn of abi.functions) {
    if (descriptors.get(fn.name) !== 'function') throw new Error(`artifact is missing ${fn.name}`);
    if (typeof exports[fn.name] !== 'function' || exports[fn.name].length !== fn.parameters.length) {
      throw new Error(`artifact has the wrong arity for ${fn.name}`);
    }
  }
}
