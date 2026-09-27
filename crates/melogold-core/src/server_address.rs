//! Адрес сервера, который ввёл человек (API §7.1, векторы `spec/server-address.vectors.json`,
//! Windows `ServerAddressPolicy.cs`): trim; без схемы — `https://`; схема и хост в нижнем регистре;
//! `/` в конце убирается, префикс пути остаётся; логин, параметры и фрагмент — отказ. `https` — на
//! любой хост, `http` — только на частный. Проверку после DNS делает вызывающий код
//! ([`is_private_ip`]), здесь имена не резолвятся.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerAddress {
    /// `scheme://host[:port][/prefix]` без `/` в конце.
    pub url: String,
    pub insecure: bool,
}

/// Код отказа: `empty | malformed | unsupported_scheme | credentials_or_params | https_required`.
pub fn normalize(input: &str) -> Result<ServerAddress, &'static str> {
    let text = input.trim();
    if text.is_empty() {
        return Err("empty");
    }
    let has_scheme = text.find("://").is_some_and(|end| {
        end > 0
            && text[..end].chars().enumerate().all(|(i, c)| c.is_ascii_alphabetic() || (i > 0 && (c.is_ascii_digit() || "+.-".contains(c))))
    });
    let with_scheme = if has_scheme { text.to_owned() } else { format!("https://{text}") };
    let scheme_end = with_scheme.find("://").ok_or("malformed")?;
    let scheme = with_scheme[..scheme_end].to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err("unsupported_scheme");
    }
    let rest = &with_scheme[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if authority.contains('@') || tail.contains('?') || tail.contains('#') {
        return Err("credentials_or_params");
    }
    if authority.is_empty() {
        return Err("malformed");
    }
    let (host, port_text) = if let Some(inner) = authority.strip_prefix('[') {
        let close = inner.find(']').ok_or("malformed")?;
        let address = &inner[..close];
        let after = &inner[close + 1..];
        if !after.is_empty() && !after.starts_with(':') {
            return Err("malformed");
        }
        address.parse::<Ipv6Addr>().map_err(|_| "malformed")?;
        (format!("[{}]", address.to_ascii_lowercase()), after.strip_prefix(':').unwrap_or("").to_owned())
    } else {
        let (host, port) = match authority.rfind(':') {
            Some(colon) => (&authority[..colon], &authority[colon + 1..]),
            None => (authority, ""),
        };
        let host = host.to_ascii_lowercase();
        if !valid_host_name(&host) {
            return Err("malformed");
        }
        (host, port.to_owned())
    };
    let port = if port_text.is_empty() {
        String::new()
    } else {
        let number: u32 = port_text.parse().ok().filter(|_| port_text.bytes().all(|b| b.is_ascii_digit())).ok_or("malformed")?;
        if !(1..=65535).contains(&number) {
            return Err("malformed");
        }
        format!(":{number}")
    };
    if tail.chars().any(char::is_whitespace) {
        return Err("malformed");
    }
    let path = tail.trim_end_matches('/');
    let insecure = scheme == "http";
    if insecure && !is_private_host(&host) {
        return Err("https_required");
    }
    Ok(ServerAddress { url: format!("{scheme}://{host}{port}{path}"), insecure })
}

/// Имя или IPv4 без скобок: метки `[a-z0-9-]`, числовой хост — только верный IPv4.
fn valid_host_name(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    if host.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) {
        return parse_ipv4(host).is_some();
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |label: &&str| {
        !label.is_empty()
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    // Последняя метка имени начинается с буквы: «1.2.3» — не имя.
    labels.iter().all(label_ok) && labels.last().and_then(|l| l.bytes().next()).is_some_and(|b| b.is_ascii_lowercase())
}

fn parse_ipv4(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (octet, part) in octets.iter_mut().zip(&parts) {
        if part.is_empty() || part.len() > 3 {
            return None;
        }
        *octet = part.parse::<u16>().ok().filter(|v| *v <= 255)? as u8;
    }
    Some(Ipv4Addr::from(octets))
}

/// Хост, на который можно по `http`: частные IPv4 и IPv6 (в скобках), `*.local`, `*.lan`,
/// `*.home.arpa`, `*.internal` или имя из одного слова.
pub fn is_private_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<Ipv6Addr>().is_ok_and(|ip| is_private_ip(IpAddr::V6(ip)));
    }
    if host.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) {
        return parse_ipv4(host).is_some_and(|ip| is_private_ip(IpAddr::V4(ip)));
    }
    !host.contains('.') || [".local", ".lan", ".home.arpa", ".internal"].iter().any(|s| host.ends_with(s) && host.len() > s.len())
}

/// Адрес частной сети — для проверки после DNS: при `http` все адреса имени должны быть такими.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 10
                || a == 127
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254)
                || (a == 100 && (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/server-address.vectors.json")).unwrap();
        let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
        let cases = spec["cases"].as_array().unwrap();
        assert!(cases.len() > 30);
        for case in cases {
            let expected = &case["expected"];
            let result = normalize(case["input"].as_str().unwrap());
            match expected["error"].as_str() {
                Some(code) => assert_eq!(result, Err(code), "{}", case["id"]),
                None => assert_eq!(
                    result,
                    Ok(ServerAddress {
                        url: expected["url"].as_str().unwrap().to_owned(),
                        insecure: expected["insecure"].as_bool().unwrap()
                    }),
                    "{}",
                    case["id"]
                ),
            }
        }
    }

    #[test]
    fn private_after_dns() {
        assert!(is_private_ip("192.168.0.5".parse().unwrap()));
        assert!(is_private_ip("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("2001:db8::1".parse().unwrap()));
    }
}
