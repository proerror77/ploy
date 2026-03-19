use std::fmt::Write;

use crate::ai_clients::prompt_sanitization::sanitize_for_llm_prompt;

use super::{PlayerStatus, StructuredGameData};

/// Format structured data for Claude analysis
pub fn format_for_claude(data: &StructuredGameData) -> String {
    let mut output = String::new();

    let _ = writeln!(
        output,
        "## Game: {} vs {}",
        sanitize_for_llm_prompt(&data.game_info.team1),
        sanitize_for_llm_prompt(&data.game_info.team2)
    );
    let _ = writeln!(
        output,
        "League: {}\n",
        sanitize_for_llm_prompt(&data.game_info.league)
    );

    append_player_section(&mut output, &data.game_info.team1, &data.team1_players);
    append_player_section(&mut output, &data.game_info.team2, &data.team2_players);

    output.push_str("\n## Betting Lines\n");
    let _ = writeln!(
        output,
        "- Spread: {} {}",
        sanitize_for_llm_prompt(&data.betting_lines.spread_team),
        data.betting_lines.spread
    );
    let _ = writeln!(
        output,
        "- Moneyline: Fav {} / Dog {}",
        data.betting_lines.moneyline_favorite, data.betting_lines.moneyline_underdog
    );
    let _ = writeln!(output, "- O/U: {}", data.betting_lines.over_under);
    let _ = writeln!(
        output,
        "- Implied Win Prob: {:.1}%",
        data.betting_lines.implied_probability * 100.0
    );
    if let Some(ref movement) = data.betting_lines.line_movement {
        let _ = writeln!(
            output,
            "- Line Movement: {}",
            sanitize_for_llm_prompt(movement)
        );
    }

    output.push_str("\n## Market Sentiment\n");
    let _ = writeln!(
        output,
        "- Expert Pick: {} ({:.0}% confidence)",
        sanitize_for_llm_prompt(&data.sentiment.expert_pick),
        data.sentiment.expert_confidence * 100.0
    );
    let _ = writeln!(
        output,
        "- Public: {:.0}% on favorite",
        data.sentiment.public_bet_percentage
    );
    let _ = writeln!(
        output,
        "- Sharp Money: {}",
        sanitize_for_llm_prompt(&data.sentiment.sharp_money_side)
    );
    let _ = writeln!(
        output,
        "- Social: {}",
        sanitize_for_llm_prompt(&data.sentiment.social_sentiment)
    );

    if !data.sentiment.key_narratives.is_empty() {
        output.push_str("\nKey Narratives:\n");
        for narrative in &data.sentiment.key_narratives {
            let _ = writeln!(output, "- {}", sanitize_for_llm_prompt(narrative));
        }
    }

    output
}

fn append_player_section(output: &mut String, team_name: &str, players: &[PlayerStatus]) {
    let _ = writeln!(
        output,
        "## {} Key Players",
        sanitize_for_llm_prompt(team_name)
    );
    for player in players {
        let ppg = player.last_5_games_ppg.unwrap_or(0.0);
        let rpg = player.last_5_games_rpg.unwrap_or(0.0);
        let apg = player.last_5_games_apg.unwrap_or(0.0);
        let _ = write!(
            output,
            "- {} | Status: {:?} | Last 5: {:.1}/{:.1}/{:.1}",
            sanitize_for_llm_prompt(&player.name),
            player.status,
            ppg,
            rpg,
            apg
        );
        if let Some(ref injury) = player.injury {
            let _ = write!(output, " ({})", sanitize_for_llm_prompt(injury));
        }
        output.push('\n');
    }
    output.push('\n');
}
