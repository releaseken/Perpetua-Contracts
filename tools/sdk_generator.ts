import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

(async () => {
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");
const abiPath = path.join(repoRoot, "contracts", "stream", "abi", "fluxora_stream.json");
const outputPath = path.join(repoRoot, "tools", "sdk", "generated", "perpetua-events.ts");

const abiJson = JSON.parse(await fs.readFile(abiPath, "utf8")) as {
  events: Array<{
    name: string;
    topics: Array<{ name: string; type: string }>;
    data: Array<{ name: string; type: string }>;
  }>;
};

const eventSpecs = abiJson.events.map((event) => ({
  name: event.name,
  topics: event.topics ?? [],
  data: event.data ?? [],
}));

function tsType(type: string): string {
  switch (type) {
    case "Address":
      return "string";
    case "u64":
    case "u32":
      return "bigint | number";
    case "i128":
    case "i64":
    case "i32":
      return "bigint | number";
    case "bool":
      return "boolean";
    case "String":
    case "symbol":
      return "string";
    default:
      return "unknown";
  }
}

function toInterface(eventName: string, eventSpec: { topics: Array<{ name: string; type: string }>; data: Array<{ name: string; type: string }> }) {
  const topicFields = eventSpec.topics
    .map((field) => `    ${field.name}: ${tsType(field.type)};`)
    .join("\n");
  const dataFields = eventSpec.data
    .map((field) => `    ${field.name}: ${tsType(field.type)};`)
    .join("\n");

  return `export interface ${eventName}Event {
  name: "${eventName}";
  topics: {
${topicFields || "    [key: string]: unknown;"}
  };
  data: {
${dataFields || "    [key: string]: unknown;"}
  };
}
`;
}

const interfaces = eventSpecs
  .map(({ name, topics, data }) => toInterface(name, { topics, data }))
  .join("\n");

const union = `export type PerpetuaEvent = ${eventSpecs.map(({ name }) => `${name}Event`).join(" | ")};`;

const generated = `import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Spec } from "@stellar/stellar-sdk/contract";

${interfaces}

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const wasmCandidates = [
  path.resolve(repoRoot, "contracts/stream/target/wasm32v1-none/release/fluxora_stream.wasm"),
  path.resolve(repoRoot, "contracts/stream/target/wasm32v1-none/release/stream.wasm"),
];

export type RpcEventValue = string | Record<string, unknown> | unknown[] | { type: string; value?: unknown };
export type RpcTopicValue = string | number | bigint | boolean | null | { type: string; value?: unknown };

const EVENT_SPECS = ${JSON.stringify(eventSpecs, null, 2)} as const;
const EVENT_NAMES = EVENT_SPECS.map((event) => event.name);

let cachedSpec: Spec | null | undefined;

async function loadContractSpec(): Promise<Spec | undefined> {
  if (cachedSpec !== undefined) {
    return cachedSpec ?? undefined;
  }

  for (const candidate of wasmCandidates) {
    try {
      const wasm = await fs.readFile(candidate);
      cachedSpec = Spec.fromWasm(new Uint8Array(wasm));
      return cachedSpec;
    } catch {
      // A compiled Wasm may not exist in the workspace yet; this module still
      // exposes the typed ABI definitions for callers using the checked-in spec.
    }
  }

  cachedSpec = null;
  return undefined;
}

function decodeFromAbi(name: string, topics: RpcTopicValue[], value: RpcEventValue): Record<string, unknown> | undefined {
  const eventSpec = EVENT_SPECS.find((spec) => spec.name === name);
  if (!eventSpec) {
    return undefined;
  }

  const dataObject = typeof value === "object" && value !== null && !Array.isArray(value) && !("type" in value)
    ? (value as Record<string, unknown>)
    : {};

  const topicObject = Object.fromEntries(
    eventSpec.topics.map((field, index) => [field.name, topics[index] ?? undefined])
  );

  const dataObjectDecoded = Object.fromEntries(
    eventSpec.data
      .map((field) => [field.name, dataObject[field.name] ?? undefined])
      .filter(([, value]) => value !== undefined)
  );

  return {
    name,
    topics: topicObject,
    data: dataObjectDecoded,
  };
}

export async function parsePerpetuaEvent(
  eventOrName: string | { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue },
  maybeTopics?: RpcTopicValue[],
  maybeValue?: RpcEventValue,
): Promise<PerpetuaEvent | undefined> {
  const eventName = typeof eventOrName === "string" ? eventOrName : (eventOrName.name ?? "");
  const topics = typeof eventOrName === "string" ? (maybeTopics ?? []) : (eventOrName.topics ?? []);
  const value = typeof eventOrName === "string" ? (maybeValue ?? {}) : (eventOrName.data ?? eventOrName.value ?? {});

  const spec = await loadContractSpec();
  if (spec) {
    try {
      const parsed = spec.parseEvent(topics as any, value as any) as { name?: string; data?: Record<string, unknown> } | undefined;
      if (parsed && parsed.name && EVENT_NAMES.includes(parsed.name)) {
        return parsed as PerpetuaEvent;
      }
    } catch {
      // Fall back to the generated ABI mapping when the contract Wasm is not built.
    }
  }

  if (!eventName) {
    return undefined;
  }

  const decoded = decodeFromAbi(eventName, topics, value);
  return decoded as PerpetuaEvent | undefined;
}

${eventSpecs.map(({ name, topics, data }) => {
  return `export function decode${name}Event(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): ${name}Event | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "${name}";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "${name}") {
    return undefined;
  }

  return {
    name: "${name}",
    topics: Object.fromEntries(${JSON.stringify(topics)}.map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries(${JSON.stringify(data)}.map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as ${name}Event;
}
`;
}).join("\n")}

${union}

export const PERPETUA_EVENT_NAMES = EVENT_NAMES;
`;

await fs.mkdir(path.dirname(outputPath), { recursive: true });
await fs.writeFile(outputPath, generated, "utf8");

console.log(`Generated event decoders at ${path.relative(repoRoot, outputPath)}`);
})();
