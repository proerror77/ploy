//! Core types for the Order Platform

use serde::{Deserialize, Serialize};

use std::str::FromStr;

/// 領域類型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Domain {
    /// 體育賽事 (NBA, NFL, etc.)
    Sports,
    /// 加密貨幣 (BTC, ETH, SOL 15分鐘輪)
    Crypto,
    /// 政治事件
    Politics,
    /// 經濟指標
    Economics,
    /// 自定義領域
    Custom(u32),
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Domain::Sports => write!(f, "Sports"),
            Domain::Crypto => write!(f, "Crypto"),
            Domain::Politics => write!(f, "Politics"),
            Domain::Economics => write!(f, "Economics"),
            Domain::Custom(id) => write!(f, "Custom({})", id),
        }
    }
}

impl FromStr for Domain {
    type Err = &'static str;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err("domain is empty");
        }

        if let Some(custom) = normalized.strip_prefix("custom:") {
            let id = custom
                .trim()
                .parse::<u32>()
                .map_err(|_| "custom domain id must be a non-negative integer")?;
            return Ok(Domain::Custom(id));
        }

        match normalized.as_str() {
            "crypto" => Ok(Domain::Crypto),
            "sports" => Ok(Domain::Sports),
            "politics" => Ok(Domain::Politics),
            "economics" => Ok(Domain::Economics),
            _ => Err("invalid domain; expected crypto|sports|politics|economics|custom:<id>"),
        }
    }
}

impl<'de> Deserialize<'de> for Domain {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct DomainVisitor;

        impl<'de> de::Visitor<'de> for DomainVisitor {
            type Value = Domain;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a domain string like \"crypto\" or \"custom:42\"")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Domain, E> {
                Domain::from_str(v).map_err(de::Error::custom)
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Domain, A::Error> {
                // Handle derived-Serialize format: {"Custom": 42}
                if let Some(key) = map.next_key::<String>()? {
                    if key.eq_ignore_ascii_case("custom") {
                        let id: u32 = map.next_value()?;
                        return Ok(Domain::Custom(id));
                    }
                    // Try as a known domain name (shouldn't happen, but be safe)
                    return Domain::from_str(&key).map_err(de::Error::custom);
                }
                Err(de::Error::custom("empty map for Domain"))
            }
        }

        deserializer.deserialize_any(DomainVisitor)
    }
}

impl Domain {
    pub fn parse_optional(raw: Option<&str>, default: Domain) -> std::result::Result<Self, String> {
        match raw {
            None => Ok(default),
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    Ok(default)
                } else {
                    Self::from_str(trimmed).map_err(|e| e.to_string())
                }
            }
        }
    }
}
