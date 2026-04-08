/**
 * Polymarket MCP tools.
 *
 * Exposes family-aware search and normalized market snapshots so operator
 * prompts can reason about crypto and sports markets with one contract.
 */

import { tool, createSdkMcpServer } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

const CLOB_BASE = "https://clob.polymarket.com";
const GAMMA_BASE = "https://gamma-api.polymarket.com";

type MarketFamily = "crypto" | "sports";
type SettlementSource = "chainlink" | "official_polymarket";

function buildUrl(path: string, params: Record<string, string | number | boolean | undefined>): string {
  const url = new URL(path, `${GAMMA_BASE}/`);
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === "") continue;
    url.searchParams.set(key, String(value));
  }
  return url.toString();
}

async function fetchJson(url: string): Promise<any> {
  const resp = await fetch(url);
  if (!resp.ok) {
    throw new Error(`HTTP ${resp.status} for ${url}`);
  }
  return resp.json();
}

function parseJsonArray(value: unknown): unknown[] {
  if (Array.isArray(value)) return value;
  if (typeof value !== "string" || value.length === 0) return [];
  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function normalizeText(value: string | null | undefined): string {
  return (value || "").trim().toLowerCase();
}

function inferCryptoSymbol(text: string): string | null {
  const upper = text.toUpperCase();
  if (upper.includes("BITCOIN") || upper.includes("BTC")) return "BTCUSDT";
  if (upper.includes("ETHEREUM") || upper.includes("ETH")) return "ETHUSDT";
  if (upper.includes("SOLANA") || upper.includes("SOL ")) return "SOLUSDT";
  if (upper.includes("XRP")) return "XRPUSDT";
  if (upper.includes("DOGECOIN") || upper.includes("DOGE")) return "DOGEUSDT";
  if (upper.includes("HYPE")) return "HYPEUSDT";
  if (upper.includes("BNB") || upper.includes("BINANCE COIN")) return "BNBUSDT";
  return null;
}

function mapChainlinkSymbol(symbol: string | null): string | null {
  if (!symbol) return null;
  const base = symbol.replace(/USDT$/i, "").toLowerCase();
  return `${base}/usd`;
}

function inferFamily(event: any, market: any): MarketFamily {
  if (
    market?.sportsMarketType ||
    market?.gameId ||
    event?.gameId ||
    event?.sportsradarMatchId ||
    event?.homeTeamName ||
    event?.awayTeamName
  ) {
    return "sports";
  }
  return "crypto";
}

function normalizeSemantics(family: MarketFamily, market: any): string {
  if (family === "crypto") return "updown";

  const raw = normalizeText(market?.sportsMarketType);
  if (raw === "moneyline") return "moneyline";
  if (raw.includes("spread")) return "spread";
  if (raw.includes("total")) return "total";
  if (raw === "yesno" || raw === "yes_no") return "yesno";
  return "unknown";
}

function normalizeDescriptor(market: any, event?: any) {
  const family = inferFamily(event, market);
  const question = market?.question || event?.title || null;
  const strategySymbol = family === "crypto" ? inferCryptoSymbol(question || "") : null;
  const outcomePrices = parseJsonArray(market?.outcomePrices);
  const tokenIds = parseJsonArray(market?.clobTokenIds).map(String);
  const settlementSource: SettlementSource =
    family === "crypto" ? "chainlink" : "official_polymarket";

  return {
    market_family: family,
    event_id: event?.id || market?.gameId || null,
    event_slug: event?.slug || null,
    market_id: String(market?.id || market?.conditionId || market?.slug || ""),
    market_slug: market?.slug || null,
    title: question,
    strategy_symbol: strategySymbol,
    reference_symbol:
      family === "crypto"
        ? mapChainlinkSymbol(strategySymbol)
        : event?.sportsradarMatchId || market?.gameId || null,
    settlement_source: settlementSource,
    league:
      event?.subcategory ||
      market?.subcategory ||
      event?.category ||
      market?.category ||
      null,
    sport:
      family === "crypto"
        ? "crypto"
        : event?.category || market?.category || event?.subcategory || market?.subcategory || null,
    start_time: market?.eventStartTime || event?.startTime || market?.startDate || event?.startDate || null,
    end_time: market?.endDate || event?.endDate || null,
    token_ids: tokenIds,
    market_semantics: normalizeSemantics(family, market),
    home_team: event?.homeTeamName || null,
    away_team: event?.awayTeamName || null,
    active: market?.active ?? event?.active ?? null,
    accepting_orders: market?.acceptingOrders ?? null,
    condition_id: market?.conditionId || null,
    price_yes: outcomePrices.length > 0 ? outcomePrices[0] : null,
    price_no: outcomePrices.length > 1 ? outcomePrices[1] : null,
    volume: market?.volumeNum ?? market?.volume ?? null,
    liquidity: market?.liquidityNum ?? market?.liquidity ?? null,
  };
}

function matchesSearch(
  descriptor: ReturnType<typeof normalizeDescriptor>,
  args: {
    query?: string;
    family?: "any" | "crypto" | "sports";
    league?: string;
    team?: string;
    slug?: string;
  }
): boolean {
  if (args.family && args.family !== "any" && descriptor.market_family !== args.family) {
    return false;
  }

  if (args.slug) {
    const target = normalizeText(args.slug);
    const marketSlug = normalizeText(descriptor.market_slug);
    const eventSlug = normalizeText(descriptor.event_slug);
    if (marketSlug !== target && eventSlug !== target) {
      return false;
    }
  }

  if (args.league) {
    if (!normalizeText(descriptor.league).includes(normalizeText(args.league))) {
      return false;
    }
  }

  if (args.team) {
    const target = normalizeText(args.team);
    const teamFields = [
      descriptor.home_team,
      descriptor.away_team,
      descriptor.title,
      descriptor.market_slug,
      descriptor.event_slug,
    ]
      .filter(Boolean)
      .map(normalizeText);

    if (!teamFields.some((value) => value.includes(target))) {
      return false;
    }
  }

  if (args.query) {
    const target = normalizeText(args.query);
    const searchable = [
      descriptor.title,
      descriptor.market_slug,
      descriptor.event_slug,
      descriptor.home_team,
      descriptor.away_team,
      descriptor.league,
      descriptor.sport,
      descriptor.strategy_symbol,
      descriptor.reference_symbol,
    ]
      .filter(Boolean)
      .map(normalizeText);

    if (!searchable.some((value) => value.includes(target))) {
      return false;
    }
  }

  return true;
}

async function fetchEventSearch(query: string, limit: number): Promise<any[]> {
  const url = buildUrl("events", {
    active: true,
    limit,
    title: query,
  });
  const events = (await fetchJson(url)) as any[];
  return events.flatMap((event) =>
    (event.markets || []).map((market: any) => ({
      descriptor: normalizeDescriptor(market, event),
      raw_event: event,
      raw_market: market,
    }))
  );
}

async function fetchBroadMarketSearch(limit: number): Promise<any[]> {
  const url = buildUrl("markets", {
    closed: false,
    limit,
  });
  const markets = (await fetchJson(url)) as any[];
  return markets.map((market) => ({
    descriptor: normalizeDescriptor(market),
    raw_event: null,
    raw_market: market,
  }));
}

async function resolveGammaMarket(args: {
  market_id?: string;
  market_slug?: string;
  condition_id?: string;
}): Promise<{ descriptor: ReturnType<typeof normalizeDescriptor> | null; raw_market: any | null }> {
  if (args.market_id) {
    const market = await fetchJson(`${GAMMA_BASE}/markets/${encodeURIComponent(args.market_id)}`);
    return { descriptor: normalizeDescriptor(market), raw_market: market };
  }

  if (args.market_slug) {
    const results = await fetchJson(
      buildUrl("markets", {
        closed: false,
        limit: 1,
        slug: args.market_slug,
      })
    );
    const market = Array.isArray(results) ? results[0] : null;
    return {
      descriptor: market ? normalizeDescriptor(market) : null,
      raw_market: market,
    };
  }

  return { descriptor: null, raw_market: null };
}

export const polymarketServer = createSdkMcpServer({
  name: "polymarket",
  version: "2.0.0",
  tools: [
    tool(
      "search_markets",
      "Search Polymarket using a normalized discovery contract. Supports generic query search plus family-aware sports lookup by team, league, or slug.",
      {
        query: z.string().optional().describe("Free-text search query"),
        family: z
          .enum(["any", "crypto", "sports"])
          .optional()
          .describe("Restrict results to a market family"),
        league: z.string().optional().describe("Sports league filter (e.g. NBA, LaLiga)"),
        team: z.string().optional().describe("Team filter for sports markets"),
        slug: z.string().optional().describe("Exact market or event slug"),
        limit: z.number().min(1).max(50).optional().describe("Max results (default 10)"),
      },
      async (args) => {
        const limit = args.limit || 10;
        const broadSearchLimit = Math.max(limit * 5, 50);

        try {
          let results: Array<{
            descriptor: ReturnType<typeof normalizeDescriptor>;
            raw_event: any;
            raw_market: any;
          }> = [];

          if (args.query) {
            results = await fetchEventSearch(args.query, broadSearchLimit);
          }

          const needsBroadSearch =
            results.length === 0 || args.family === "sports" || !!args.league || !!args.team || !!args.slug;

          if (needsBroadSearch) {
            const broadResults = await fetchBroadMarketSearch(broadSearchLimit);
            const deduped = new Map<string, typeof broadResults[number]>();
            for (const item of [...results, ...broadResults]) {
              deduped.set(item.descriptor.market_id, item);
            }
            results = [...deduped.values()];
          }

          const filtered = results
            .filter((item) => matchesSearch(item.descriptor, args))
            .slice(0, limit)
            .map((item) => ({
              ...item.descriptor,
              raw_condition_id: item.raw_market?.conditionId || null,
            }));

          return {
            content: [
              {
                type: "text" as const,
                text: JSON.stringify(
                  {
                    filters: {
                      query: args.query || null,
                      family: args.family || "any",
                      league: args.league || null,
                      team: args.team || null,
                      slug: args.slug || null,
                    },
                    count: filtered.length,
                    results: filtered,
                  },
                  null,
                  2
                ),
              },
            ],
          };
        } catch (e: any) {
          return {
            content: [{ type: "text" as const, text: `Polymarket search failed: ${e.message}` }],
            isError: true,
          };
        }
      }
    ),

    tool(
      "market_snapshot",
      "Get a normalized Polymarket market snapshot. Can resolve by condition_id, market_id, or market_slug, and optionally fetch a token order book.",
      {
        condition_id: z.string().optional().describe("Polymarket condition ID"),
        market_id: z.string().optional().describe("Gamma market ID"),
        market_slug: z.string().optional().describe("Gamma market slug"),
        token_id: z.string().optional().describe("Specific token ID for order book lookup"),
      },
      async (args) => {
        try {
          const gamma = await resolveGammaMarket(args);
          const conditionId = args.condition_id || gamma.descriptor?.condition_id || null;

          let clobMarket: any = null;
          if (conditionId) {
            const marketResp = await fetch(`${CLOB_BASE}/markets/${conditionId}`);
            if (marketResp.ok) {
              clobMarket = await marketResp.json();
            }
          }

          let orderbook = null;
          if (args.token_id) {
            const obResp = await fetch(`${CLOB_BASE}/book?token_id=${args.token_id}`);
            if (obResp.ok) {
              orderbook = await obResp.json();
            }
          }

          const bids = orderbook?.bids || [];
          const asks = orderbook?.asks || [];

          return {
            content: [
              {
                type: "text" as const,
                text: JSON.stringify(
                  {
                    descriptor: gamma.descriptor,
                    clob_snapshot: clobMarket
                      ? {
                          condition_id: clobMarket.condition_id,
                          question: clobMarket.question,
                          active: clobMarket.active,
                          closed: clobMarket.closed,
                          minimum_order_size: clobMarket.minimum_order_size,
                          minimum_tick_size: clobMarket.minimum_tick_size,
                          tokens: clobMarket.tokens,
                        }
                      : null,
                    orderbook: orderbook
                      ? {
                          token_id: args.token_id,
                          best_bid: bids.length > 0 ? bids[0].price : null,
                          best_ask: asks.length > 0 ? asks[0].price : null,
                          bid_depth: bids.slice(0, 5),
                          ask_depth: asks.slice(0, 5),
                          spread:
                            bids.length > 0 && asks.length > 0
                              ? (
                                  parseFloat(asks[0].price) - parseFloat(bids[0].price)
                                ).toFixed(4)
                              : null,
                        }
                      : null,
                  },
                  null,
                  2
                ),
              },
            ],
          };
        } catch (e: any) {
          return {
            content: [
              {
                type: "text" as const,
                text: `Market snapshot failed: ${e.message}`,
              },
            ],
            isError: true,
          };
        }
      }
    ),
  ],
});
