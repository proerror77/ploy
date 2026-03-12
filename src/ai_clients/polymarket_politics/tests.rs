use super::*;

#[test]
fn test_politics_keyword_detection() {
    let market = PolymarketPoliticsMarket {
        condition_id: "test".to_string(),
        question: Some("Trump positive favorability on April 1?".to_string()),
        slug: None,
        active: true,
        closed: false,
        end_date: None,
        clob_token_ids: None,
        outcome_prices: None,
        volume: None,
        liquidity: None,
        description: None,
        tags: vec![],
    };

    assert!(market.is_politics_market());
}

#[test]
fn test_category_matching() {
    let market = PolymarketPoliticsMarket {
        condition_id: "test".to_string(),
        question: Some("Biden approval rating above 40%?".to_string()),
        slug: None,
        active: true,
        closed: false,
        end_date: None,
        clob_token_ids: None,
        outcome_prices: None,
        volume: None,
        liquidity: None,
        description: None,
        tags: vec![],
    };

    assert!(market.matches_category(PoliticalCategory::Approval));
    assert!(market.matches_category(PoliticalCategory::All));
}

#[test]
fn test_subject_extraction() {
    let market = PolymarketPoliticsMarket {
        condition_id: "test".to_string(),
        question: Some("Will Trump win 2024 election?".to_string()),
        slug: None,
        active: true,
        closed: false,
        end_date: None,
        clob_token_ids: None,
        outcome_prices: None,
        volume: None,
        liquidity: None,
        description: None,
        tags: vec![],
    };

    let subject = market.extract_subject();
    assert!(subject.is_some());
    assert_eq!(subject.unwrap(), "trump");
}

#[test]
fn test_token_id_parsing() {
    let market = PolymarketPoliticsMarket {
        condition_id: "test".to_string(),
        question: None,
        slug: None,
        active: true,
        closed: false,
        end_date: None,
        clob_token_ids: Some(r#"["token1", "token2"]"#.to_string()),
        outcome_prices: Some(r#"["0.52", "0.48"]"#.to_string()),
        volume: None,
        liquidity: None,
        description: None,
        tags: vec![],
    };

    let tokens = market.get_token_ids();
    assert!(tokens.is_some());
    let (yes, no) = tokens.unwrap();
    assert_eq!(yes, "token1");
    assert_eq!(no, "token2");

    let prices = market.get_prices();
    assert!(prices.is_some());
}
