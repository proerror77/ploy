/**
 * Polymarket MCP Tools — Direct API calls to Polymarket CLOB.
 *
 * Provides market search and snapshot capabilities for the sidecar.
 * These hit the public Polymarket API directly.
 */

import { tool, createSdkMcpServer } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

const CLOB_BASE = "https://clob.polymarket.com";
const GAMMA_BASE = "https://gamma-api.polymarket.com";

export const polymarketServer = createSdkMcpServer({
  name: "polymarket",
  version: "1.0.0",
  tools: [
    tool(
      "search_markets",
      "Search Polymarket for active markets matching a query. Returns market slugs, questions, prices, and condition IDs.",
      {
        query: z.string().describe("Search query (e.g., 'Lakers vs Celtics', 'NBA champion')"),
        limit: z.number().min(1).max(20).optional().describe("Max results (default 10)"),
      },
      async (args) => {
        const limit = args.limit || 10;

        try {
          const resp = await fetch(
            `${GAMMA_BASE}/events?active=true&limit=${limit}&title=${encodeURIComponent(args.query)}`
          );

          if (!resp.ok) {
            return {
              content: [{ type: "text" as const, text: `Polymarket search error: ${resp.status}` }],
              isError: true,
            };
          }

          const events = await resp.json();

          const results = (events as any[]).map((event: any) => ({
            event_id: event.id,
            title: event.title,
            slug: event.slug,
            active: event.active,
            end_date: event.endDate,
            markets: (event.markets || []).map((m: any) => ({
              condition_id: m.conditionId,
              question: m.question,
              slug: m.slug,
              outcome_yes_price: m.outcomePrices
                ? JSON.parse(m.outcomePrices)[0]
                : null,
              outcome_no_price: m.outcomePrices
                ? JSON.parse(m.outcomePrices)[1]
                : null,
              volume: m.volume,
              liquidity: m.liquidity,
            })),
          }));

          return {
            content: [
              {
                type: "text" as const,
                text: JSON.stringify({ query: args.query, count: results.length, results }, null, 2),
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
      "Get detailed snapshot of a specific Polymarket market. Returns best bid/ask, last trade price, and order book depth.",
      {
        condition_id: z
          .string()
          .describe("Polymarket condition ID (hex string)"),
        token_id: z
          .string()
          .optional()
          .describe("Specific token ID to get order book for"),
      },
      async (args) => {
        try {
          // Get market info
          const marketResp = await fetch(
            `${CLOB_BASE}/markets/${args.condition_id}`
          );

          if (!marketResp.ok) {
            return {
              content: [
                {
                  type: "text" as const,
                  text: `Market not found: ${marketResp.status}`,
                },
              ],
              isError: true,
            };
          }

          const market = await marketResp.json();

          // Get order book if token_id provided
          let orderbook = null;
          if (args.token_id) {
            const obResp = await fetch(
              `${CLOB_BASE}/book?token_id=${args.token_id}`
            );
            if (obResp.ok) {
              orderbook = await obResp.json();
            }
          }

          const snapshot: any = {
            condition_id: market.condition_id,
            question: market.question,
            tokens: market.tokens,
            active: market.active,
            closed: market.closed,
            minimum_order_size: market.minimum_order_size,
            minimum_tick_size: market.minimum_tick_size,
          };

          if (orderbook) {
            const bids = orderbook.bids || [];
            const asks = orderbook.asks || [];
            snapshot.orderbook = {
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
            };
          }

          return {
            content: [
              {
                type: "text" as const,
                text: JSON.stringify(snapshot, null, 2),
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
