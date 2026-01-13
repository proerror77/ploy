#!/usr/bin/env python3
"""
Sports Betting Analysis Skill for Claude Agent SDK

This skill provides AI-powered sports betting analysis by:
1. Collecting data from multiple sources (Grok)
2. Analyzing with Claude Opus
3. Generating trade recommendations

Usage in Agent SDK:
    from skills.sports_bet import sports_bet_skill

    result = await sports_bet_skill(
        url="https://polymarket.com/event/nba-phi-dal-2026-01-11",
        compare_draftkings=False,
        min_edge=5.0
    )
"""

import asyncio
import json
import os
import subprocess
from typing import Dict, Any, Optional


async def sports_bet_skill(
    url: str,
    compare_draftkings: bool = False,
    min_edge: float = 5.0
) -> Dict[str, Any]:
    """
    Analyze a sports betting opportunity using AI.

    Args:
        url: Polymarket event URL
        compare_draftkings: Include DraftKings odds comparison
        min_edge: Minimum edge percentage to recommend

    Returns:
        Dictionary containing analysis results:
        {
            "game": {"league": str, "team1": str, "team2": str},
            "market_odds": {...},
            "prediction": {...},
            "recommendation": {...},
            "draftkings": {...} (optional),
            "success": bool,
            "error": str (if failed)
        }
    """

    # Check required environment variables
    required_vars = ["GROK_API_KEY", "ANTHROPIC_API_KEY"]
    missing_vars = [var for var in required_vars if not os.getenv(var)]

    if missing_vars:
        return {
            "success": False,
            "error": f"Missing required environment variables: {', '.join(missing_vars)}",
            "help": "Set GROK_API_KEY and ANTHROPIC_API_KEY in your environment"
        }

    if compare_draftkings and not os.getenv("THE_ODDS_API_KEY"):
        return {
            "success": False,
            "error": "THE_ODDS_API_KEY required for DraftKings comparison",
            "help": "Get a free API key at https://the-odds-api.com/"
        }

    try:
        # Call the Rust implementation via CLI
        cmd = ["ploy", "sports", "analyze", "--url", url]

        if compare_draftkings:
            # Use the Chain command which includes DK comparison
            # Extract team names from URL
            parts = url.split("/")[-1].split("-")
            if len(parts) >= 3:
                team1 = parts[1].upper()
                team2 = parts[2].upper()
                league = parts[0].upper()

                cmd = [
                    "ploy", "sports", "chain",
                    "--team1", team1,
                    "--team2", team2,
                    "--sport", league
                ]

        # Execute command and capture output
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=120  # 2 minute timeout
        )

        if result.returncode != 0:
            return {
                "success": False,
                "error": f"Analysis failed: {result.stderr}",
                "stdout": result.stdout
            }

        # Parse the output
        output = result.stdout
        analysis = parse_analysis_output(output)

        # Check if edge meets minimum threshold
        if analysis.get("recommendation", {}).get("edge", 0) < min_edge:
            analysis["warning"] = f"Edge ({analysis['recommendation']['edge']:.1f}%) below minimum threshold ({min_edge}%)"

        analysis["success"] = True
        return analysis

    except subprocess.TimeoutExpired:
        return {
            "success": False,
            "error": "Analysis timed out after 2 minutes"
        }
    except Exception as e:
        return {
            "success": False,
            "error": f"Unexpected error: {str(e)}"
        }


def parse_analysis_output(output: str) -> Dict[str, Any]:
    """
    Parse the CLI output into structured data.

    This is a simple parser - in production you'd want to use JSON output
    from the CLI for more reliable parsing.
    """

    lines = output.split("\n")
    analysis = {
        "game": {},
        "market_odds": {},
        "prediction": {},
        "recommendation": {},
        "key_factors": []
    }

    current_section = None

    for line in lines:
        line = line.strip()

        # Detect sections
        if "GAME INFORMATION" in line:
            current_section = "game"
        elif "MARKET ODDS" in line:
            current_section = "market_odds"
        elif "AI PREDICTION" in line:
            current_section = "prediction"
        elif "TRADE RECOMMENDATION" in line:
            current_section = "recommendation"
        elif "Key Factors:" in line:
            current_section = "key_factors"

        # Parse data based on section
        if current_section == "game":
            if "League:" in line:
                analysis["game"]["league"] = line.split("League:")[-1].strip()
            elif "Teams:" in line:
                teams = line.split("Teams:")[-1].strip().split(" vs ")
                if len(teams) == 2:
                    analysis["game"]["team1"] = teams[0].strip()
                    analysis["game"]["team2"] = teams[1].strip()

        elif current_section == "prediction":
            if "Win Probability:" in line:
                prob = extract_percentage(line)
                if "team1" not in analysis["prediction"]:
                    analysis["prediction"]["team1_win_prob"] = prob
                else:
                    analysis["prediction"]["team2_win_prob"] = prob
            elif "Confidence:" in line:
                analysis["prediction"]["confidence"] = extract_percentage(line)

        elif current_section == "recommendation":
            if "Action:" in line:
                analysis["recommendation"]["action"] = line.split("Action:")[-1].strip()
            elif "Side:" in line:
                analysis["recommendation"]["side"] = line.split("Side:")[-1].strip()
            elif "Edge:" in line:
                analysis["recommendation"]["edge"] = extract_percentage(line)
            elif "Suggested Size:" in line:
                analysis["recommendation"]["suggested_size"] = extract_percentage(line)

        elif current_section == "key_factors":
            if line.startswith("•") or line.startswith("-"):
                analysis["key_factors"].append(line[1:].strip())

    return analysis


def extract_percentage(text: str) -> float:
    """Extract percentage value from text like '58.5%' or '+13.5%'"""
    import re
    match = re.search(r'([+-]?\d+\.?\d*)%', text)
    if match:
        return float(match.group(1))
    return 0.0


# Tool definition for Claude Agent SDK
TOOL_DEFINITION = {
    "name": "sports_bet_analysis",
    "description": "Analyze sports betting opportunities on Polymarket using AI-powered multi-source analysis. Provides win probability predictions, edge calculations, and trade recommendations.",
    "input_schema": {
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "Polymarket event URL (e.g., https://polymarket.com/event/nba-phi-dal-2026-01-11)"
            },
            "compare_draftkings": {
                "type": "boolean",
                "description": "Include DraftKings odds comparison for arbitrage detection",
                "default": False
            },
            "min_edge": {
                "type": "number",
                "description": "Minimum edge percentage to recommend (default: 5.0)",
                "default": 5.0
            }
        },
        "required": ["url"]
    }
}


async def handle_tool_call(tool_input: Dict[str, Any]) -> Dict[str, Any]:
    """
    Handle tool call from Claude Agent SDK.

    This is the main entry point when Claude calls this tool.
    """
    url = tool_input.get("url")
    compare_draftkings = tool_input.get("compare_draftkings", False)
    min_edge = tool_input.get("min_edge", 5.0)

    if not url:
        return {
            "success": False,
            "error": "URL parameter is required"
        }

    result = await sports_bet_skill(
        url=url,
        compare_draftkings=compare_draftkings,
        min_edge=min_edge
    )

    return result


# Example usage
if __name__ == "__main__":
    async def main():
        # Test the skill
        result = await sports_bet_skill(
            url="https://polymarket.com/event/nba-phi-dal-2026-01-11",
            compare_draftkings=False,
            min_edge=5.0
        )

        print(json.dumps(result, indent=2))

    asyncio.run(main())
