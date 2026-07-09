import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

export type GrokBuilderContext = {
  provider: "xai";
  model: string;
  summary: string;
};

type FetchLike = typeof fetch;

export async function queryGrokBuilderContext(params: {
  objective: string;
  runPacket: string;
  runContract: string;
  fetchImpl?: FetchLike;
}): Promise<GrokBuilderContext | null> {
  const apiKey = process.env.XAI_API_KEY?.trim() || process.env.GROK_API_KEY?.trim();
  if (!apiKey) return null;

  const model = process.env.XAI_MODEL || "grok-4.5";
  const endpoint = process.env.XAI_CHAT_COMPLETIONS_URL || "https://api.x.ai/v1/chat/completions";
  const fetchImpl = params.fetchImpl ?? fetch;
  const response = await fetchImpl(endpoint, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${apiKey}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      model,
      temperature: 0.2,
      messages: [
        {
          role: "system",
          content:
            "You are Grok Builder evidence synthesis for a trading harness. Return compact evidence only. Do not recommend live orders.",
        },
        {
          role: "user",
          content: [
            `Objective:\n${params.objective}`,
            `Run packet:\n${params.runPacket}`,
            `Run contract:\n${params.runContract}`,
            "Return: grok_decision candidate (trade/pass/not_queried), evidence gaps, and confidence.",
          ].join("\n\n"),
        },
      ],
    }),
  });
  if (!response.ok) {
    throw new Error(`xAI Grok API failed: ${response.status} ${await response.text()}`);
  }

  const body = (await response.json()) as {
    choices?: Array<{ message?: { content?: unknown } }>;
  };
  const summary = body.choices?.[0]?.message?.content;
  return {
    provider: "xai",
    model,
    summary: typeof summary === "string" && summary.trim() ? summary : "Grok returned no text",
  };
}

async function selfTest() {
  process.env.XAI_API_KEY = "test-key";
  process.env.XAI_MODEL = "grok-test";
  const result = await queryGrokBuilderContext({
    objective: "test",
    runPacket: "packet",
    runContract: "contract",
    fetchImpl: (async (_url, init) => {
      assert.match(String(init?.body), /grok_decision candidate/);
      return new Response(
        JSON.stringify({
          choices: [{ message: { content: "grok_decision candidate: pass" } }],
        }),
        { status: 200 }
      );
    }) as FetchLike,
  });
  assert.equal(result?.model, "grok-test");
  assert.match(result?.summary ?? "", /pass/);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
