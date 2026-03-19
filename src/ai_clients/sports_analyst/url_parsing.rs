use super::*;

impl SportsAnalyst {
    /// Parse event URL to extract slug, league, and team names.
    pub(super) fn parse_event_url(&self, url: &str) -> Result<(String, String, String, String)> {
        let slug = url
            .split("/event/")
            .nth(1)
            .ok_or_else(|| PloyError::Internal("Invalid event URL format".into()))?
            .split('?')
            .next()
            .unwrap_or("")
            .to_string();

        if slug.contains('/') {
            let url_parts: Vec<&str> = slug.split('/').collect();
            let league = url_parts[0]
                .split('-')
                .next()
                .unwrap_or("NBA")
                .to_uppercase();

            if url_parts.len() > 1 {
                let matchup = url_parts[1];
                if let Some(vs_pos) = matchup.find("-vs-") {
                    let team1_slug = &matchup[..vs_pos];
                    let team2_part = &matchup[vs_pos + 4..];
                    let team2_slug = self.extract_team_name(team2_part);
                    let team1 = self.slug_to_team_name(team1_slug, &league);
                    let team2 = self.slug_to_team_name(&team2_slug, &league);
                    return Ok((slug, league, team1, team2));
                }
            }

            return Err(PloyError::Internal(
                "Cannot parse teams from long URL format".into(),
            ));
        }

        let parts: Vec<&str> = slug.split('-').collect();
        if parts.len() < 3 {
            return Err(PloyError::Internal("Cannot parse teams from URL".into()));
        }

        let league = parts[0].to_uppercase();
        let team1 = self.expand_team_code(parts[1], &league);
        let team2 = self.expand_team_code(parts[2], &league);

        Ok((slug, league, team1, team2))
    }

    pub(super) fn extract_team_name(&self, slug: &str) -> String {
        let months = [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ];

        let parts: Vec<&str> = slug.split('-').collect();
        let mut team_parts = Vec::new();

        for part in parts {
            if months.contains(&part.to_lowercase().as_str()) || part.parse::<u32>().is_ok() {
                break;
            }
            team_parts.push(part);
        }

        team_parts.join("-")
    }

    pub(super) fn slug_to_team_name(&self, slug: &str, league: &str) -> String {
        let parts: Vec<&str> = slug.split('-').collect();

        if let Some(last) = parts.last() {
            let code = if last.chars().all(|c| c.is_numeric()) {
                slug.to_string()
            } else {
                last.to_string()
            };

            let expanded = self.expand_team_code(&code, league);
            if expanded != code.to_uppercase() {
                return expanded;
            }
        }

        slug.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => String::new(),
                }
            })
            .collect::<Vec<String>>()
            .join(" ")
    }

    pub(super) fn expand_team_code(&self, code: &str, league: &str) -> String {
        let code_upper = code.to_uppercase();

        match league {
            "NBA" => match code_upper.as_str() {
                "PHI" => "Philadelphia 76ers".to_string(),
                "DAL" => "Dallas Mavericks".to_string(),
                "LAL" => "Los Angeles Lakers".to_string(),
                "BOS" => "Boston Celtics".to_string(),
                "MIA" => "Miami Heat".to_string(),
                "GSW" => "Golden State Warriors".to_string(),
                "DEN" => "Denver Nuggets".to_string(),
                "MIL" => "Milwaukee Bucks".to_string(),
                "PHX" => "Phoenix Suns".to_string(),
                "MEM" => "Memphis Grizzlies".to_string(),
                "CLE" => "Cleveland Cavaliers".to_string(),
                "NYK" => "New York Knicks".to_string(),
                "SAC" => "Sacramento Kings".to_string(),
                "LAC" => "Los Angeles Clippers".to_string(),
                "MIN" => "Minnesota Timberwolves".to_string(),
                "NOP" => "New Orleans Pelicans".to_string(),
                "ATL" => "Atlanta Hawks".to_string(),
                "CHI" => "Chicago Bulls".to_string(),
                "TOR" => "Toronto Raptors".to_string(),
                "BKN" => "Brooklyn Nets".to_string(),
                "OKC" => "Oklahoma City Thunder".to_string(),
                "IND" => "Indiana Pacers".to_string(),
                "HOU" => "Houston Rockets".to_string(),
                "ORL" => "Orlando Magic".to_string(),
                "POR" => "Portland Trail Blazers".to_string(),
                "UTA" => "Utah Jazz".to_string(),
                "SAS" => "San Antonio Spurs".to_string(),
                "WAS" => "Washington Wizards".to_string(),
                "CHA" => "Charlotte Hornets".to_string(),
                "DET" => "Detroit Pistons".to_string(),
                _ => code_upper,
            },
            "NFL" => match code_upper.as_str() {
                "KC" | "KCC" => "Kansas City Chiefs".to_string(),
                "SF" | "SFO" => "San Francisco 49ers".to_string(),
                "BUF" => "Buffalo Bills".to_string(),
                "BAL" => "Baltimore Ravens".to_string(),
                "GB" | "GNB" => "Green Bay Packers".to_string(),
                "DET" => "Detroit Lions".to_string(),
                "TB" | "TBB" => "Tampa Bay Buccaneers".to_string(),
                "PHI" => "Philadelphia Eagles".to_string(),
                "DAL" => "Dallas Cowboys".to_string(),
                "MIA" => "Miami Dolphins".to_string(),
                "NYJ" => "New York Jets".to_string(),
                "NYG" => "New York Giants".to_string(),
                "NE" | "NEP" => "New England Patriots".to_string(),
                "LAR" => "Los Angeles Rams".to_string(),
                "LAC" => "Los Angeles Chargers".to_string(),
                "DEN" => "Denver Broncos".to_string(),
                "LV" | "LVR" => "Las Vegas Raiders".to_string(),
                "MIN" => "Minnesota Vikings".to_string(),
                "CHI" => "Chicago Bears".to_string(),
                "SEA" => "Seattle Seahawks".to_string(),
                "ARI" => "Arizona Cardinals".to_string(),
                "ATL" => "Atlanta Falcons".to_string(),
                "CAR" => "Carolina Panthers".to_string(),
                "NO" | "NOS" => "New Orleans Saints".to_string(),
                "CIN" => "Cincinnati Bengals".to_string(),
                "CLE" => "Cleveland Browns".to_string(),
                "PIT" => "Pittsburgh Steelers".to_string(),
                "IND" => "Indianapolis Colts".to_string(),
                "JAX" => "Jacksonville Jaguars".to_string(),
                "TEN" => "Tennessee Titans".to_string(),
                "HOU" => "Houston Texans".to_string(),
                "WAS" => "Washington Commanders".to_string(),
                _ => code_upper,
            },
            _ => code_upper,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_analyst() -> SportsAnalyst {
        let grok = GrokClient::new(crate::ai_clients::grok::GrokConfig::default()).unwrap();
        let claude = ClaudeAgentClient::new();
        SportsAnalyst::new(grok, claude)
    }

    #[test]
    fn test_expand_team_code() {
        let analyst = create_test_analyst();
        assert_eq!(analyst.expand_team_code("phi", "NBA"), "Philadelphia 76ers");
        assert_eq!(analyst.expand_team_code("DAL", "NBA"), "Dallas Mavericks");
        assert_eq!(analyst.expand_team_code("LAL", "NBA"), "Los Angeles Lakers");
        assert_eq!(analyst.expand_team_code("DET", "NBA"), "Detroit Pistons");
        assert_eq!(
            analyst.expand_team_code("phi", "NFL"),
            "Philadelphia Eagles"
        );
        assert_eq!(analyst.expand_team_code("DAL", "NFL"), "Dallas Cowboys");
        assert_eq!(analyst.expand_team_code("DET", "NFL"), "Detroit Lions");
    }

    #[test]
    fn test_parse_event_url() {
        let analyst = create_test_analyst();
        let (slug, league, team1, team2) = analyst
            .parse_event_url("https://polymarket.com/event/nba-phi-dal-2026-01-01")
            .unwrap();

        assert_eq!(slug, "nba-phi-dal-2026-01-01");
        assert_eq!(league, "NBA");
        assert_eq!(team1, "Philadelphia 76ers");
        assert_eq!(team2, "Dallas Mavericks");
    }

    #[test]
    fn test_parse_nfl_event() {
        let analyst = create_test_analyst();
        let (slug, league, team1, team2) = analyst
            .parse_event_url("https://polymarket.com/event/nfl-kc-sf-2026-02-09")
            .unwrap();

        assert_eq!(slug, "nfl-kc-sf-2026-02-09");
        assert_eq!(league, "NFL");
        assert_eq!(team1, "Kansas City Chiefs");
        assert_eq!(team2, "San Francisco 49ers");
    }

    #[test]
    fn test_parse_long_format_url() {
        let analyst = create_test_analyst();
        let (slug, league, team1, team2) = analyst
            .parse_event_url("https://polymarket.com/event/nba-regular-season-2024-2025/philadelphia-76ers-vs-dallas-mavericks-jan-2-2025")
            .unwrap();

        assert_eq!(league, "NBA");
        assert_eq!(team1, "Philadelphia 76ers");
        assert_eq!(team2, "Dallas Mavericks");
        assert!(slug.contains("philadelphia-76ers-vs-dallas-mavericks"));
    }

    #[test]
    fn test_extract_team_name() {
        let analyst = create_test_analyst();
        assert_eq!(
            analyst.extract_team_name("dallas-mavericks-jan-2-2025"),
            "dallas-mavericks"
        );
        assert_eq!(
            analyst.extract_team_name("golden-state-warriors-dec-25-2024"),
            "golden-state-warriors"
        );
    }

    #[test]
    fn test_slug_to_team_name() {
        let analyst = create_test_analyst();
        assert_eq!(
            analyst.slug_to_team_name("dallas-mavericks", "NBA"),
            "Dallas Mavericks"
        );
        assert_eq!(
            analyst.slug_to_team_name("philadelphia-76ers", "NBA"),
            "Philadelphia 76ers"
        );
    }
}
