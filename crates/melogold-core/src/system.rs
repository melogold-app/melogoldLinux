//! Сведения о системе для журнала, «Диагностики» и `DeviceInput` (docs/PROMPT.md §3 «Идентичность»).

use std::path::Path;

/// `PRETTY_NAME` из `/etc/os-release` (или `/usr/lib/os-release`), иначе `Linux`.
pub fn os_pretty_name() -> String {
    ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_os_release(&text, "PRETTY_NAME"))
        .unwrap_or_else(|| "Linux".into())
}

/// Значение поля os-release: `KEY=value`, `KEY="value"` или `KEY='value'`.
pub fn parse_os_release(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        if name != key {
            return None;
        }
        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        let unescaped = unquoted.replace("\\\"", "\"").replace("\\\\", "\\");
        (!unescaped.is_empty()).then_some(unescaped)
    })
}

/// Модель компьютера из DMI, если читается и не заглушка производителя.
pub fn product_name() -> Option<String> {
    let name = read_trimmed(Path::new("/sys/class/dmi/id/product_name"))?;
    let placeholder = ["System Product Name", "To be filled by O.E.M.", "Default string", "None"];
    (!placeholder.iter().any(|p| name.eq_ignore_ascii_case(p))).then_some(name)
}

/// Имя компьютера: «красивое» из `/etc/machine-info` (его пишет `hostnamectl`), иначе hostname.
pub fn device_name() -> String {
    std::fs::read_to_string("/etc/machine-info")
        .ok()
        .and_then(|text| parse_os_release(&text, "PRETTY_HOSTNAME"))
        .or_else(|| read_trimmed(Path::new("/etc/hostname")))
        .or_else(|| read_trimmed(Path::new("/proc/sys/kernel/hostname")))
        .unwrap_or_else(|| "Linux".into())
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_values() {
        let text = "NAME=\"Fedora Linux\"\nPRETTY_NAME=\"Fedora Linux 44 (Workstation Edition)\"\nID=fedora\nVERSION_ID='44'\n";
        assert_eq!(parse_os_release(text, "PRETTY_NAME").as_deref(), Some("Fedora Linux 44 (Workstation Edition)"));
        assert_eq!(parse_os_release(text, "ID").as_deref(), Some("fedora"));
        assert_eq!(parse_os_release(text, "VERSION_ID").as_deref(), Some("44"));
        assert_eq!(parse_os_release(text, "MISSING"), None);
    }
}
