/**
 * ESPN MCP Tools — Direct API calls to ESPN for live NBA game data.
 *
 * These tools call the public ESPN API directly from TypeScript,
 * so no Rust backend dependency is needed for game data.
 */

import { tool, createSdkMcpServer } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

const ESPN_BASE = "https://site.api.espn.com/apis/site/v2/sports/basketball/nba";

interface EspnCompetitor {
  team: { abbreviation: string; displayName: string };
  score: string;
  homeAway: string;
}

interface EspnCompetition {
  competitors: EspnCompetitor[];
  status: {
    displayClock: string;
    period: number;
    type: { description: string; completed: boolean };
  };
}

interface EspnEvent {
  id: string;
  name: string;
  competitions: EspnCompetition[];
}

interface EspnScoreboard {
  events: EspnEvent[];
}

function formatGame(event: EspnEvent) {
  const comp = event.competitions[0];
  if (!comp) return null;

  const home = comp.competitors.find((c) => c.homeAway === "home");
  const away = comp.competitors.find((c) => c.homeAway === "away");
  if (!home || !away) return null;

  const homeScore = parseInt(home.score || "0", 10);
  const awayScore = parseInt(away.score || "0", 10);
  const deficit = Math.abs(homeScore - awayScore);
  const trailing =
    homeScore < awayScore ? home.team.abbreviation : away.team.abbreviation;
  const leading =
    homeScore >= awayScore ? home.team.abbreviation : away.team.abbreviation;

  return {
    game_id: event.id,
    name: event.name,
    home_team: home.team.displayName,
    home_abbrev: home.team.abbreviation,
    away_team: away.team.displayName,
    away_abbrev: away.team.abbreviation,
    home_score: homeScore,
    away_score: awayScore,
    quarter: comp.status.period,
    clock: comp.status.displayClock,
    status: comp.status.type.description,
    completed: comp.status.type.completed,
    deficit,
    trailing_team: trailing,
    leading_team: leading,
  };
}

export const espnServer = createSdkMcpServer({
  name: "espn",
  version: "1.0.0",
  tools: [
    tool(
      "scoreboard",
      "Get live NBA scoreboard with all games for today (or a specific date). Returns game IDs, scores, quarter, clock, and trailing team info.",
      {
        date: z
          .string()
          .optional()
          .describe("Date in YYYYMMDD format. Defaults to today."),
      },
      async (args) => {
        const date =
          args.date ||
          new Date().toISOString().slice(0, 10).replace(/-/g, "");
        const resp = await fetch(
          `${ESPN_BASE}/scoreboard?dates=${date}`
        );

        if (!resp.ok) {
          return {
            content: [
              { type: "text" as const, text: `ESPN API error: ${resp.status}` },
            ],
            isError: true,
          };
        }

        const data = (await resp.json()) as EspnScoreboard;
        const games = (data.events || []).map(formatGame).filter(Boolean);

        return {
          content: [
            {
              type: "text" as const,
              text: JSON.stringify(
                {
                  date,
                  game_count: games.length,
                  games,
                },
                null,
                2
              ),
            },
          ],
        };
      }
    ),

    tool(
      "game_details",
      "Get detailed info for a specific NBA game by ESPN game ID. Includes quarter scores, play-by-play summary, and team stats.",
      {
        game_id: z.string().describe("ESPN game ID (e.g., '401584701')"),
      },
      async (args) => {
        const resp = await fetch(
          `${ESPN_BASE}/summary?event=${args.game_id}`
        );

        if (!resp.ok) {
          return {
            content: [
              {
                type: "text" as const,
                text: `ESPN API error: ${resp.status} for game ${args.game_id}`,
              },
            ],
            isError: true,
          };
        }

        const data = await resp.json();
        return {
          content: [
            {
              type: "text" as const,
              text: JSON.stringify(data, null, 2).slice(0, 8000),
            },
          ],
        };
      }
    ),
  ],
});
