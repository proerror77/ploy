use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct LocalBook {
    pub symbol: String,
    pub last_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    synced: bool,
}

#[derive(Debug, Clone)]
pub struct BookSnapshot {
    pub symbol: String,
    pub last_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone)]
pub struct BookDiff {
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalBookError {
    FirstDiffOutOfSequence {
        snapshot_last_update_id: u64,
        first_update_id: u64,
        final_update_id: u64,
    },
    DiffGap {
        expected_first_update_id: u64,
        actual_first_update_id: u64,
        final_update_id: u64,
    },
}

impl fmt::Display for LocalBookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FirstDiffOutOfSequence {
                snapshot_last_update_id,
                first_update_id,
                final_update_id,
            } => write!(
                f,
                "first Binance futures diff is out of sequence: snapshot last_update_id={snapshot_last_update_id}, diff U={first_update_id}, u={final_update_id}"
            ),
            Self::DiffGap {
                expected_first_update_id,
                actual_first_update_id,
                final_update_id,
            } => write!(
                f,
                "Binance futures diff gap: expected U={expected_first_update_id}, got U={actual_first_update_id}, u={final_update_id}"
            ),
        }
    }
}

impl std::error::Error for LocalBookError {}

impl LocalBook {
    pub fn from_snapshot(snapshot: BookSnapshot) -> Self {
        let mut book = Self {
            symbol: snapshot.symbol,
            last_update_id: snapshot.last_update_id,
            bids: snapshot.bids,
            asks: snapshot.asks,
            synced: false,
        };
        sort_book(&mut book.bids, Side::Bid);
        sort_book(&mut book.asks, Side::Ask);
        book
    }

    pub fn apply_diff(&mut self, diff: BookDiff) -> Result<(), LocalBookError> {
        let expected = self.last_update_id + 1;
        let sequence_ok = if self.synced {
            diff.first_update_id == expected && diff.final_update_id >= expected
        } else {
            diff.first_update_id <= expected && expected <= diff.final_update_id
        };
        if sequence_ok {
            apply_levels(&mut self.bids, &diff.bids, Side::Bid);
            apply_levels(&mut self.asks, &diff.asks, Side::Ask);
            self.last_update_id = diff.final_update_id;
            self.synced = true;
            return Ok(());
        }

        if !self.synced {
            return Err(LocalBookError::FirstDiffOutOfSequence {
                snapshot_last_update_id: self.last_update_id,
                first_update_id: diff.first_update_id,
                final_update_id: diff.final_update_id,
            });
        }

        Err(LocalBookError::DiffGap {
            expected_first_update_id: expected,
            actual_first_update_id: diff.first_update_id,
            final_update_id: diff.final_update_id,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Side {
    Bid,
    Ask,
}

fn apply_levels(levels: &mut Vec<BookLevel>, updates: &[BookLevel], side: Side) {
    for update in updates {
        if let Some(existing) = levels.iter_mut().find(|level| level.price == update.price) {
            existing.size = update.size;
        } else if update.size > 0.0 {
            levels.push(update.clone());
        }
    }
    levels.retain(|level| level.size > 0.0);
    sort_book(levels, side);
}

fn sort_book(levels: &mut [BookLevel], side: Side) {
    levels.sort_by(|lhs, rhs| match side {
        Side::Bid => rhs.price.partial_cmp(&lhs.price).unwrap_or(Ordering::Equal),
        Side::Ask => lhs.price.partial_cmp(&rhs.price).unwrap_or(Ordering::Equal),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> BookSnapshot {
        BookSnapshot {
            symbol: "BTCUSDT".to_string(),
            last_update_id: 100,
            bids: vec![BookLevel {
                price: 100.0,
                size: 2.0,
            }],
            asks: vec![BookLevel {
                price: 101.0,
                size: 3.0,
            }],
        }
    }

    #[test]
    fn first_diff_must_bridge_snapshot_update_id() {
        let mut book = LocalBook::from_snapshot(snapshot());
        book.apply_diff(BookDiff {
            first_update_id: 99,
            final_update_id: 101,
            bids: vec![BookLevel {
                price: 100.0,
                size: 1.5,
            }],
            asks: vec![],
        })
        .expect("bridging first diff");

        assert_eq!(book.last_update_id, 101);
        assert_eq!(book.bids[0].size, 1.5);
    }

    #[test]
    fn first_diff_rejects_when_it_does_not_cover_snapshot_next_id() {
        let mut book = LocalBook::from_snapshot(snapshot());
        let err = book
            .apply_diff(BookDiff {
                first_update_id: 102,
                final_update_id: 103,
                bids: vec![],
                asks: vec![],
            })
            .expect_err("sequence gap");

        assert_eq!(
            err,
            LocalBookError::FirstDiffOutOfSequence {
                snapshot_last_update_id: 100,
                first_update_id: 102,
                final_update_id: 103,
            }
        );
    }

    #[test]
    fn later_diff_must_start_after_previous_final_update_id() {
        let mut book = LocalBook::from_snapshot(snapshot());
        book.apply_diff(BookDiff {
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![],
            asks: vec![],
        })
        .expect("first diff");

        let err = book
            .apply_diff(BookDiff {
                first_update_id: 103,
                final_update_id: 103,
                bids: vec![],
                asks: vec![],
            })
            .expect_err("later gap");

        assert_eq!(
            err,
            LocalBookError::DiffGap {
                expected_first_update_id: 102,
                actual_first_update_id: 103,
                final_update_id: 103,
            }
        );
    }

    #[test]
    fn later_diff_rejects_overlap_even_when_it_covers_expected_id() {
        let mut book = LocalBook::from_snapshot(snapshot());
        book.apply_diff(BookDiff {
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![],
            asks: vec![],
        })
        .expect("first diff");

        let err = book
            .apply_diff(BookDiff {
                first_update_id: 101,
                final_update_id: 102,
                bids: vec![],
                asks: vec![],
            })
            .expect_err("later overlap");

        assert_eq!(
            err,
            LocalBookError::DiffGap {
                expected_first_update_id: 102,
                actual_first_update_id: 101,
                final_update_id: 102,
            }
        );
    }

    #[test]
    fn applies_replacements_removals_and_sorting() {
        let mut book = LocalBook::from_snapshot(snapshot());
        book.apply_diff(BookDiff {
            first_update_id: 101,
            final_update_id: 101,
            bids: vec![
                BookLevel {
                    price: 100.0,
                    size: 0.0,
                },
                BookLevel {
                    price: 99.5,
                    size: 4.0,
                },
                BookLevel {
                    price: 100.5,
                    size: 1.0,
                },
            ],
            asks: vec![
                BookLevel {
                    price: 101.0,
                    size: 2.0,
                },
                BookLevel {
                    price: 100.8,
                    size: 1.0,
                },
            ],
        })
        .expect("diff");

        assert_eq!(
            book.bids,
            vec![
                BookLevel {
                    price: 100.5,
                    size: 1.0,
                },
                BookLevel {
                    price: 99.5,
                    size: 4.0,
                },
            ]
        );
        assert_eq!(book.asks[0].price, 100.8);
        assert_eq!(book.asks[1].price, 101.0);
        assert_eq!(book.asks[1].size, 2.0);
    }
}
