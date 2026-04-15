#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Regime {
    Early,   // 181..=300s
    Middle,  // 61..=180s
    Late,    // 6..=60s
    Expiry,  // 0..=5s
}

impl Regime {
    pub fn from_secs(t: i64) -> Self {
        match t {
            181..=300 => Regime::Early,
            61..=180  => Regime::Middle,
            6..=60    => Regime::Late,
            _         => Regime::Expiry,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Regime::Early  => "early",
            Regime::Middle => "middle",
            Regime::Late   => "late",
            Regime::Expiry => "expiry",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorMeta {
    pub name: String,
    pub regime: Regime,
    pub label: String,
    pub ic: f64,
    pub direction: i8,
    pub stability: f64,
}

pub struct FactorRegistry { factors: Vec<FactorMeta> }

impl FactorRegistry {
    pub fn new() -> Self { Self { factors: Vec::new() } }

    pub fn insert(&mut self, meta: FactorMeta) { self.factors.push(meta); }

    pub fn top_n(&self, regime: Regime, label: &str, n: usize) -> Vec<&FactorMeta> {
        let mut v: Vec<&FactorMeta> = self.factors.iter()
            .filter(|m| m.regime == regime && m.label == label)
            .collect();
        v.sort_by(|a, b| b.ic.abs().partial_cmp(&a.ic.abs()).unwrap());
        v.truncate(n);
        v
    }

    pub fn for_regime(&self, regime: Regime) -> Vec<&FactorMeta> {
        let mut v: Vec<&FactorMeta> = self.factors.iter()
            .filter(|m| m.regime == regime).collect();
        v.sort_by(|a, b| b.ic.abs().partial_cmp(&a.ic.abs()).unwrap());
        v
    }

    pub fn all(&self) -> &[FactorMeta] { &self.factors }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regime_from_time_remaining() {
        assert_eq!(Regime::from_secs(290), Regime::Early);
        assert_eq!(Regime::from_secs(120), Regime::Middle);
        assert_eq!(Regime::from_secs(30),  Regime::Late);
        assert_eq!(Regime::from_secs(3),   Regime::Expiry);
    }

    #[test]
    fn registry_top_n_sorted_by_abs_ic() {
        let mut reg = FactorRegistry::new();
        reg.insert(FactorMeta { name: "a".into(), regime: Regime::Early,
            label: "settlement_up".into(), ic: 0.05, direction: 1, stability: 0.8 });
        reg.insert(FactorMeta { name: "b".into(), regime: Regime::Early,
            label: "settlement_up".into(), ic: 0.15, direction: -1, stability: 1.2 });
        let top = reg.top_n(Regime::Early, "settlement_up", 1);
        assert_eq!(top[0].name, "b");
    }
}
