import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Spec } from "@stellar/stellar-sdk/contract";

export interface CancelledEvent {
  name: "Cancelled";
  topics: {
    stream_id: bigint | number;
    sender: string;
    recipient: string;
  };
  data: {
    refunded: bigint | number;
    vested: bigint | number;
    withdrawn: bigint | number;
    end_time: bigint | number;
  };
}

export interface PausedEvent {
  name: "Paused";
  topics: {
    stream_id: bigint | number;
    sender: string;
  };
  data: {
    paused_at: bigint | number;
    paused_total: bigint | number;
  };
}

export interface RecipientTransferredEvent {
  name: "RecipientTransferred";
  topics: {
    stream_id: bigint | number;
    old_recipient: string;
    new_recipient: string;
  };
  data: {
    [key: string]: unknown;
  };
}

export interface ResumedEvent {
  name: "Resumed";
  topics: {
    stream_id: bigint | number;
    sender: string;
  };
  data: {
    paused_duration: bigint | number;
    paused_total: bigint | number;
  };
}

export interface StreamCreatedEvent {
  name: "StreamCreated";
  topics: {
    stream_id: bigint | number;
    sender: string;
    recipient: string;
  };
  data: {
    token: string;
    deposited: bigint | number;
    start_time: bigint | number;
    end_time: bigint | number;
    cliff_time: bigint | number;
    cancellable: boolean;
    pausable: boolean;
    transferable: boolean;
  };
}

export interface ToppedUpEvent {
  name: "ToppedUp";
  topics: {
    stream_id: bigint | number;
    sender: string;
  };
  data: {
    amount: bigint | number;
    deposited: bigint | number;
    end_time: bigint | number;
  };
}

export interface TtlExtendedEvent {
  name: "TtlExtended";
  topics: {
    stream_id: bigint | number;
  };
  data: {
    extended_to_ledgers: bigint | number;
  };
}

export interface WithdrawnEvent {
  name: "Withdrawn";
  topics: {
    stream_id: bigint | number;
    recipient: string;
  };
  data: {
    amount: bigint | number;
    withdrawn: bigint | number;
    deposited: bigint | number;
    status: unknown;
  };
}


const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../../..");
const wasmCandidates = [
  path.resolve(repoRoot, "contracts/stream/target/wasm32v1-none/release/fluxora_stream.wasm"),
  path.resolve(repoRoot, "contracts/stream/target/wasm32v1-none/release/stream.wasm"),
];

export type RpcEventValue = string | Record<string, unknown> | unknown[] | { type: string; value?: unknown };
export type RpcTopicValue = string | number | bigint | boolean | null | { type: string; value?: unknown };

const EVENT_SPECS = [
  {
    "name": "Cancelled",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "sender",
        "type": "Address"
      },
      {
        "name": "recipient",
        "type": "Address"
      }
    ],
    "data": [
      {
        "name": "refunded",
        "type": "i128"
      },
      {
        "name": "vested",
        "type": "i128"
      },
      {
        "name": "withdrawn",
        "type": "i128"
      },
      {
        "name": "end_time",
        "type": "u64"
      }
    ]
  },
  {
    "name": "Paused",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "sender",
        "type": "Address"
      }
    ],
    "data": [
      {
        "name": "paused_at",
        "type": "u64"
      },
      {
        "name": "paused_total",
        "type": "u64"
      }
    ]
  },
  {
    "name": "RecipientTransferred",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "old_recipient",
        "type": "Address"
      },
      {
        "name": "new_recipient",
        "type": "Address"
      }
    ],
    "data": []
  },
  {
    "name": "Resumed",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "sender",
        "type": "Address"
      }
    ],
    "data": [
      {
        "name": "paused_duration",
        "type": "u64"
      },
      {
        "name": "paused_total",
        "type": "u64"
      }
    ]
  },
  {
    "name": "StreamCreated",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "sender",
        "type": "Address"
      },
      {
        "name": "recipient",
        "type": "Address"
      }
    ],
    "data": [
      {
        "name": "token",
        "type": "Address"
      },
      {
        "name": "deposited",
        "type": "i128"
      },
      {
        "name": "start_time",
        "type": "u64"
      },
      {
        "name": "end_time",
        "type": "u64"
      },
      {
        "name": "cliff_time",
        "type": "u64"
      },
      {
        "name": "cancellable",
        "type": "bool"
      },
      {
        "name": "pausable",
        "type": "bool"
      },
      {
        "name": "transferable",
        "type": "bool"
      }
    ]
  },
  {
    "name": "ToppedUp",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "sender",
        "type": "Address"
      }
    ],
    "data": [
      {
        "name": "amount",
        "type": "i128"
      },
      {
        "name": "deposited",
        "type": "i128"
      },
      {
        "name": "end_time",
        "type": "u64"
      }
    ]
  },
  {
    "name": "TtlExtended",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      }
    ],
    "data": [
      {
        "name": "extended_to_ledgers",
        "type": "u32"
      }
    ]
  },
  {
    "name": "Withdrawn",
    "topics": [
      {
        "name": "stream_id",
        "type": "u64"
      },
      {
        "name": "recipient",
        "type": "Address"
      }
    ],
    "data": [
      {
        "name": "amount",
        "type": "i128"
      },
      {
        "name": "withdrawn",
        "type": "i128"
      },
      {
        "name": "deposited",
        "type": "i128"
      },
      {
        "name": "status",
        "type": "StreamStatus"
      }
    ]
  }
] as const;
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

export function decodeCancelledEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): CancelledEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "Cancelled";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "Cancelled") {
    return undefined;
  }

  return {
    name: "Cancelled",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"sender","type":"Address"},{"name":"recipient","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"refunded","type":"i128"},{"name":"vested","type":"i128"},{"name":"withdrawn","type":"i128"},{"name":"end_time","type":"u64"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as CancelledEvent;
}

export function decodePausedEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): PausedEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "Paused";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "Paused") {
    return undefined;
  }

  return {
    name: "Paused",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"sender","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"paused_at","type":"u64"},{"name":"paused_total","type":"u64"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as PausedEvent;
}

export function decodeRecipientTransferredEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): RecipientTransferredEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "RecipientTransferred";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "RecipientTransferred") {
    return undefined;
  }

  return {
    name: "RecipientTransferred",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"old_recipient","type":"Address"},{"name":"new_recipient","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as RecipientTransferredEvent;
}

export function decodeResumedEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): ResumedEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "Resumed";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "Resumed") {
    return undefined;
  }

  return {
    name: "Resumed",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"sender","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"paused_duration","type":"u64"},{"name":"paused_total","type":"u64"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as ResumedEvent;
}

export function decodeStreamCreatedEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): StreamCreatedEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "StreamCreated";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "StreamCreated") {
    return undefined;
  }

  return {
    name: "StreamCreated",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"sender","type":"Address"},{"name":"recipient","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"token","type":"Address"},{"name":"deposited","type":"i128"},{"name":"start_time","type":"u64"},{"name":"end_time","type":"u64"},{"name":"cliff_time","type":"u64"},{"name":"cancellable","type":"bool"},{"name":"pausable","type":"bool"},{"name":"transferable","type":"bool"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as StreamCreatedEvent;
}

export function decodeToppedUpEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): ToppedUpEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "ToppedUp";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "ToppedUp") {
    return undefined;
  }

  return {
    name: "ToppedUp",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"sender","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"amount","type":"i128"},{"name":"deposited","type":"i128"},{"name":"end_time","type":"u64"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as ToppedUpEvent;
}

export function decodeTtlExtendedEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): TtlExtendedEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "TtlExtended";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "TtlExtended") {
    return undefined;
  }

  return {
    name: "TtlExtended",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"extended_to_ledgers","type":"u32"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as TtlExtendedEvent;
}

export function decodeWithdrawnEvent(event: { name?: string; topics?: RpcTopicValue[]; data?: RpcEventValue; value?: RpcEventValue } | undefined): WithdrawnEvent | undefined {
  if (!event) {
    return undefined;
  }

  const eventName = event.name ?? "Withdrawn";
  const topics = event.topics ?? [];
  const value = event.data ?? event.value ?? {};
  const decoded = decodeFromAbi(eventName, topics, value);

  if (!decoded || decoded.name !== "Withdrawn") {
    return undefined;
  }

  return {
    name: "Withdrawn",
    topics: Object.fromEntries([{"name":"stream_id","type":"u64"},{"name":"recipient","type":"Address"}].map((field, index) => [field.name, topics[index] ?? undefined])),
    data: Object.fromEntries([{"name":"amount","type":"i128"},{"name":"withdrawn","type":"i128"},{"name":"deposited","type":"i128"},{"name":"status","type":"StreamStatus"}].map((field) => [field.name, (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>)[field.name] : undefined) ?? undefined]).filter(([, val]) => val !== undefined)),
  } as WithdrawnEvent;
}


export type PerpetuaEvent = CancelledEvent | PausedEvent | RecipientTransferredEvent | ResumedEvent | StreamCreatedEvent | ToppedUpEvent | TtlExtendedEvent | WithdrawnEvent;

export const PERPETUA_EVENT_NAMES = EVENT_NAMES;
