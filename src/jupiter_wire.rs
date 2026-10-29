//! Decode complete Borsh arguments from the pinned official Jupiter v6 ABI.
//! Never inspect an assumed suffix: Anchor can ignore trailing bytes.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
type Result<T> = std::result::Result<T, String>;
fn idl() -> &'static Value {
    static ABI: OnceLock<Value> = OnceLock::new();
    ABI.get_or_init(|| {
        serde_json::from_str(include_str!("../config/abi/jupiter-v6.json"))
            .expect("bundled ABI is valid JSON")
    })
}
struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .position
            .checked_add(n)
            .ok_or("instruction size overflow")?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or("truncated Jupiter instruction")?;
        self.position = end;
        Ok(bytes)
    }
    fn number(&mut self, n: usize) -> Result<u64> {
        let mut bytes = [0; 8];
        bytes[..n].copy_from_slice(self.take(n)?);
        Ok(u64::from_le_bytes(bytes))
    }
    fn value(&mut self, ty: &Value, depth: usize) -> Result<Option<u64>> {
        if depth > 24 {
            return Err("ABI nesting limit".into());
        }
        if let Some(name) = ty.as_str() {
            let size = match name {
                "u8" | "i8" | "bool" => 1,
                "u16" | "i16" => 2,
                "u32" | "i32" => 4,
                "u64" | "i64" => 8,
                "u128" | "i128" => 16,
                "publicKey" | "pubkey" => 32,
                _ => return Err("unsupported ABI scalar".into()),
            };
            if size <= 8 {
                let n = self.number(size)?;
                if name == "bool" && n > 1 {
                    return Err("invalid Borsh bool".into());
                }
                return Ok(Some(n));
            }
            self.take(size)?;
            return Ok(None);
        }
        if let Some(name) = ty.get("defined") {
            let name = name
                .as_str()
                .or_else(|| name["name"].as_str())
                .ok_or("invalid defined ABI type")?;
            let definition = idl()["types"]
                .as_array()
                .ok_or("missing ABI types")?
                .iter()
                .find(|t| t["name"] == name)
                .ok_or("unknown ABI type")?;
            return self.value(&definition["type"], depth + 1);
        }
        if let Some(element) = ty.get("vec") {
            let count = self.number(4)?;
            if count > 64 {
                return Err("ABI vector limit".into());
            }
            for _ in 0..count {
                self.value(element, depth + 1)?;
            }
            return Ok(None);
        }
        if let Some(element) = ty.get("option") {
            match self.number(1)? {
                0 => {}
                1 => {
                    self.value(element, depth + 1)?;
                }
                _ => return Err("invalid Borsh option".into()),
            }
            return Ok(None);
        }
        if let Some(array) = ty.get("array").and_then(Value::as_array) {
            let count = array
                .get(1)
                .and_then(Value::as_u64)
                .filter(|n| *n <= 64)
                .ok_or("invalid ABI array")?;
            for _ in 0..count {
                self.value(&array[0], depth + 1)?;
            }
            return Ok(None);
        }
        let fields = match ty["kind"].as_str() {
            Some("struct") => ty["fields"].as_array().ok_or("invalid ABI struct")?,
            Some("enum") => {
                let index = self.number(1)? as usize;
                let variant = ty["variants"]
                    .as_array()
                    .and_then(|v| v.get(index))
                    .ok_or("unknown Borsh enum variant")?;
                let Some(fields) = variant["fields"].as_array() else {
                    return Ok(None);
                };
                fields
            }
            _ => return Err("unsupported ABI shape".into()),
        };
        for field in fields {
            self.value(field.get("type").unwrap_or(field), depth + 1)?;
        }
        Ok(None)
    }
}
pub struct Route {
    pub source: usize,
    pub destination: usize,
    pub authority: usize,
    pub destination_extra: Option<usize>,
    pub input: u64,
    pub minimum: u64,
}
fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
pub fn decode(data: &[u8]) -> Result<Route> {
    let discriminator = data.get(..8).ok_or("missing Jupiter discriminator")?;
    let (name, snake) = if discriminator == &Sha256::digest(b"global:route")[..8] {
        ("route", "route")
    } else if discriminator == &Sha256::digest(b"global:shared_accounts_route")[..8] {
        ("sharedAccountsRoute", "shared_accounts_route")
    } else {
        return Err("unsupported Jupiter route variant; use a supported exact-in route".into());
    };
    let _ = snake;
    let instruction = idl()["instructions"]
        .as_array()
        .ok_or("missing ABI instructions")?
        .iter()
        .find(|i| {
            i["name"]
                .as_str()
                .is_some_and(|n| normalized(n) == normalized(name))
        })
        .ok_or("missing exact-in ABI")?;
    let mut reader = Reader { data, position: 8 };
    let mut values = std::collections::BTreeMap::new();
    for arg in instruction["args"]
        .as_array()
        .ok_or("missing ABI arguments")?
    {
        if arg["name"]
            .as_str()
            .is_some_and(|n| normalized(n) == "routeplan")
        {
            let mut peek = Reader {
                data,
                position: reader.position,
            };
            if peek.number(4)? == 0 {
                return Err("empty route plan".into());
            }
        }
        if let Some(value) = reader.value(&arg["type"], 0)? {
            values.insert(
                normalized(arg["name"].as_str().ok_or("invalid argument name")?),
                value,
            );
        }
    }
    if reader.position != data.len() {
        return Err("trailing Jupiter instruction bytes".into());
    }
    let get = |name: &str| {
        values
            .get(&normalized(name))
            .copied()
            .ok_or_else(|| format!("missing {name}"))
    };
    let slippage = get("slippageBps")?;
    if slippage > 100 || get("platformFeeBps")? != 0 {
        return Err("on-chain slippage/platform fee exceeds policy".into());
    }
    let accounts = instruction["accounts"]
        .as_array()
        .ok_or("missing route accounts")?;
    let account = |names: &[&str]| {
        accounts
            .iter()
            .position(|a| {
                a["name"]
                    .as_str()
                    .is_some_and(|actual| names.iter().any(|n| normalized(actual) == normalized(n)))
            })
            .ok_or("missing route account role".to_string())
    };
    Ok(Route {
        destination_extra: if name == "route" {
            Some(account(&["destinationTokenAccount"])?)
        } else {
            None
        },
        source: account(&["userSourceTokenAccount", "sourceTokenAccount"])?,
        destination: account(&["userDestinationTokenAccount", "destinationTokenAccount"])?,
        authority: account(&["userTransferAuthority"])?,
        input: get("inAmount")?,
        minimum: (get("quotedOutAmount")? as u128 * (10000 - slippage) as u128 / 10000) as u64,
    })
}
#[cfg(test)]
pub fn fixture(input: u64, minimum: u64) -> Vec<u8> {
    let mut bytes = Sha256::digest(b"global:route")[..8].to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend([0, 100, 0, 1]);
    bytes.extend(input.to_le_bytes());
    bytes.extend(minimum.to_le_bytes());
    bytes.extend([0, 0, 0]);
    bytes
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pumpswap_and_shared_route_frames_decode_using_the_pinned_abi() {
        for variant in [72, 73] {
            let mut data = fixture(500, 900_000);
            data[12] = variant;
            let route = decode(&data).unwrap();
            assert_eq!((route.input, route.minimum), (500, 900_000));
            let mut shared = Sha256::digest(b"global:shared_accounts_route")[..8].to_vec();
            shared.push(0);
            shared.extend(&data[8..]);
            let route = decode(&shared).unwrap();
            assert_eq!(
                (route.authority, route.source, route.destination),
                (2, 3, 6)
            );
        }
    }
    #[test]
    fn complete_borsh_decoder_rejects_fake_suffix_and_weak_limits() {
        let mut data = fixture(1_000_000, 990_000);
        let r = decode(&data).unwrap();
        assert_eq!((r.input, r.minimum), (1_000_000, 990_000));
        data.extend(fixture(1_000_000, 2_000_000));
        assert!(decode(&data).is_err());
        let mut data = fixture(1, 1);
        let n = data.len();
        data[n - 3..n - 1].copy_from_slice(&101u16.to_le_bytes());
        assert!(decode(&data).is_err());
        let mut data = fixture(1, 1);
        data[12] = 255;
        assert!(decode(&data).is_err());
    }
}
