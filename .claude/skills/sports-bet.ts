/**
 * Sports Betting Analysis Skill for Claude Agent SDK (TypeScript)
 *
 * This skill provides AI-powered sports betting analysis by:
 * 1. Collecting data from multiple sources (Grok)
 * 2. Analyzing with Claude Opus
 * 3. Generating trade recommendations
 *
 * @example
 * ```typescript
 * import { sportsBetSkill } from './skills/sports-bet';
 *
 * const result = await sportsBetSkill({
 *   url: "https://polymarket.com/event/nba-phi-dal-2026-01-11",
 *   compareDraftkings: false,
 *   minEdge: 5.0
 * });
 * ```
 */

import { exec } from 'child_process';
import { promisify } from 'util';

const execAsync = promisify(exec);

interface SportsBetInput {
  url: string;
  compareDraftkings?: boolean;
  minEdge?: number;
}

interface GameInfo {
  league: string;
  team1: string;
  team2: string;
}

interface MarketOdds {
  team1_yes: number;
  team1_no: number;
  team2_yes?: number;
  team2_no?: number;
  spread?: string;
}

interface Prediction {
  team1_win_prob: number;
  team2_win_prob: number;
  confidence: number;
  reasoning: string;
  key_factors: string[];
}

interface Recommendation {
  action: 'Buy' | 'Sell' | 'Hold' | 'Avoid';
  side: string;
  edge: number;
  suggested_size: number;
  reasoning: string;
}

interface DraftKingsComparison {
  edge: number;
  recommended_side: string;
  home_edge: number;
  away_edge: number;
}

interface SportsBetResult {
  success: boolean;
  game?: GameInfo;
  market_odds?: MarketOdds;
  prediction?: Prediction;
  recommendation?: Recommendation;
  draftkings?: DraftKingsComparison;
  warning?: string;
  error?: string;
  help?: string;
}

/**
 * Analyze a sports betting opportunity using AI
 */
export async function sportsBetSkill(
  input: SportsBetInput
): Promise<SportsBetResult> {
  const { url, compareDraftkings = false, minEdge = 5.0 } = input;

  // Check required environment variables
  const requiredVars = ['GROK_API_KEY', 'ANTHROPIC_API_KEY'];
  const missingVars = requiredVars.filter(v => !process.env[v]);

  if (missingVars.length > 0) {
    return {
      success: false,
      error: `Missing required environment variables: ${missingVars.join(', ')}`,
      help: 'Set GROK_API_KEY and ANTHROPIC_API_KEY in your environment'
    };
  }

  if (compareDraftkings && !process.env.THE_ODDS_API_KEY) {
    return {
      success: false,
      error: 'THE_ODDS_API_KEY required for DraftKings comparison',
      help: 'Get a free API key at https://the-odds-api.com/'
    };
  }

  try {
    // Call the Rust implementation via CLI with JSON output
    const cmd = [
      'ploy',
      'sports',
      'bet',
      '--url',
      url,
      '--format',
      'json'
    ];

    if (compareDraftkings) {
      cmd.push('--compare-dk');
    }

    cmd.push('--min-edge', minEdge.toString());

    // Execute command with timeout
    const { stdout, stderr } = await execAsync(cmd.join(' '), {
      timeout: 120000, // 2 minute timeout
      maxBuffer: 10 * 1024 * 1024 // 10MB buffer
    });

    if (stderr) {
      console.error('CLI stderr:', stderr);
    }

    // Parse JSON output
    const analysis = JSON.parse(stdout);

    // Check if edge meets minimum threshold
    if (analysis.recommendation?.edge < minEdge) {
      analysis.warning = `Edge (${analysis.recommendation.edge.toFixed(1)}%) below minimum threshold (${minEdge}%)`;
    }

    return {
      success: true,
      ...analysis
    };

  } catch (error: any) {
    if (error.killed) {
      return {
        success: false,
        error: 'Analysis timed out after 2 minutes'
      };
    }

    return {
      success: false,
      error: `Unexpected error: ${error.message}`
    };
  }
}

/**
 * Tool definition for Claude Agent SDK
 */
export const TOOL_DEFINITION = {
  name: 'sports_bet_analysis',
  description: 'Analyze sports betting opportunities on Polymarket using AI-powered multi-source analysis. Provides win probability predictions, edge calculations, and trade recommendations.',
  input_schema: {
    type: 'object',
    properties: {
      url: {
        type: 'string',
        description: 'Polymarket event URL (e.g., https://polymarket.com/event/nba-phi-dal-2026-01-11)'
      },
      compareDraftkings: {
        type: 'boolean',
        description: 'Include DraftKings odds comparison for arbitrage detection',
        default: false
      },
      minEdge: {
        type: 'number',
        description: 'Minimum edge percentage to recommend (default: 5.0)',
        default: 5.0
      }
    },
    required: ['url']
  }
};

/**
 * Handle tool call from Claude Agent SDK
 */
export async function handleToolCall(
  toolInput: SportsBetInput
): Promise<SportsBetResult> {
  if (!toolInput.url) {
    return {
      success: false,
      error: 'URL parameter is required'
    };
  }

  return await sportsBetSkill(toolInput);
}

/**
 * Format result for display
 */
export function formatResult(result: SportsBetResult): string {
  if (!result.success) {
    return `❌ Error: ${result.error}\n${result.help || ''}`;
  }

  const { game, market_odds, prediction, recommendation, draftkings } = result;

  let output = '═'.repeat(70) + '\n';
  output += '  AI-POWERED SPORTS BETTING ANALYSIS\n';
  output += '═'.repeat(70) + '\n\n';

  // Game info
  if (game) {
    output += `📊 GAME: ${game.team1} vs ${game.team2}\n`;
    output += `   League: ${game.league}\n\n`;
  }

  // Market odds
  if (market_odds) {
    output += '💰 MARKET ODDS (Polymarket)\n';
    output += `   ${game?.team1} YES: ${market_odds.team1_yes.toFixed(3)} (${(market_odds.team1_yes * 100).toFixed(1)}%)\n`;
    if (market_odds.team2_yes) {
      output += `   ${game?.team2} YES: ${market_odds.team2_yes.toFixed(3)} (${(market_odds.team2_yes * 100).toFixed(1)}%)\n`;
    }
    output += '\n';
  }

  // AI prediction
  if (prediction) {
    output += '🤖 AI PREDICTION (Claude Opus)\n';
    output += `   ${game?.team1} Win Probability: ${(prediction.team1_win_prob * 100).toFixed(1)}%\n`;
    output += `   ${game?.team2} Win Probability: ${(prediction.team2_win_prob * 100).toFixed(1)}%\n`;
    output += `   Confidence: ${(prediction.confidence * 100).toFixed(0)}%\n\n`;
    output += `   Reasoning: ${prediction.reasoning}\n\n`;

    if (prediction.key_factors.length > 0) {
      output += '   Key Factors:\n';
      prediction.key_factors.forEach(factor => {
        output += `     • ${factor}\n`;
      });
      output += '\n';
    }
  }

  // Recommendation
  if (recommendation) {
    const actionEmoji = {
      'Buy': '✅',
      'Sell': '❌',
      'Hold': '⏸️',
      'Avoid': '🚫'
    }[recommendation.action] || '❓';

    output += '📈 TRADE RECOMMENDATION\n';
    output += `   Action: ${actionEmoji} ${recommendation.action}\n`;
    output += `   Side: ${recommendation.side}\n`;
    output += `   Edge: ${recommendation.edge >= 0 ? '+' : ''}${recommendation.edge.toFixed(1)}%\n`;
    output += `   Suggested Size: ${recommendation.suggested_size.toFixed(1)}% of bankroll\n\n`;
    output += `   Reasoning: ${recommendation.reasoning}\n\n`;
  }

  // DraftKings comparison
  if (draftkings) {
    output += '🎲 DRAFTKINGS COMPARISON\n';
    output += `   Edge: ${draftkings.edge >= 0 ? '+' : ''}${draftkings.edge.toFixed(1)}%\n`;
    output += `   Recommended Side: ${draftkings.recommended_side}\n\n`;
  }

  // Warning
  if (result.warning) {
    output += `⚠️  ${result.warning}\n\n`;
  }

  output += '═'.repeat(70) + '\n';

  return output;
}

// Example usage
if (require.main === module) {
  (async () => {
    const result = await sportsBetSkill({
      url: 'https://polymarket.com/event/nba-phi-dal-2026-01-11',
      compareDraftkings: false,
      minEdge: 5.0
    });

    console.log(formatResult(result));
  })();
}
