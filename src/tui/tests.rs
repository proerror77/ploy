#[cfg(test)]
mod tests {
    use crate::tui::{DisplayTransaction, TuiApp};
    use rust_decimal_macros::dec;

    #[test]
    fn test_tui_app_new() {
        let app = TuiApp::new();
        assert!(app.is_running());
        assert!(app.positions.is_empty());
        assert!(!app.show_help);
    }

    #[test]
    fn test_toggle_help() {
        let mut app = TuiApp::new();
        assert!(!app.show_help);
        app.toggle_help();
        assert!(app.show_help);
        app.toggle_help();
        assert!(!app.show_help);
    }

    #[test]
    fn test_market_switching() {
        let mut app = TuiApp::new();
        // Markets are now dynamic; set them before testing switching
        app.set_markets(vec!["SOL-15m".into(), "SOL-4h".into(), "ETH-15m".into()]);
        assert_eq!(app.selected_market_idx, 0);
        assert_eq!(app.selected_market, "SOL-15m");

        app.next_market();
        assert_eq!(app.selected_market_idx, 1);
        assert_eq!(app.selected_market, "SOL-4h");

        app.prev_market();
        assert_eq!(app.selected_market_idx, 0);
        assert_eq!(app.selected_market, "SOL-15m");

        // Test wrap around
        app.prev_market();
        assert_eq!(app.selected_market_idx, 2);
        assert_eq!(app.selected_market, "ETH-15m");
    }

    #[test]
    fn test_scroll() {
        let mut app = TuiApp::new();
        app.scroll_down();
        assert_eq!(app.tx_scroll_offset, 0); // No transactions

        // Add some transactions
        for i in 0..5 {
            app.add_transaction(DisplayTransaction::new(
                chrono::Utc::now(),
                crate::domain::Side::Up,
                dec!(0.5),
                100,
                dec!(50000),
                format!("tx{}", i),
            ));
        }

        app.scroll_down();
        assert_eq!(app.tx_scroll_offset, 1);

        app.scroll_up();
        assert_eq!(app.tx_scroll_offset, 0);
    }

    #[test]
    fn test_quit() {
        let mut app = TuiApp::new();
        assert!(app.is_running());
        app.quit();
        assert!(!app.is_running());
    }
}
