//! Kleine Helfer: Sortierung, Formatierung, Platzhalter-Suche.

use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

/// Kleinschreibung plus Umlaut-Faltung, damit „Ärger" bei „A" einsortiert wird
/// und ein Filter „a" auch „ä" findet.
pub fn fold_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            'ä' | 'Ä' | 'à' | 'á' | 'â' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Å' => out.push('a'),
            'ö' | 'Ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'Ò' | 'Ó' | 'Ô' | 'Õ' => out.push('o'),
            'ü' | 'Ü' | 'ù' | 'ú' | 'û' | 'Ù' | 'Ú' | 'Û' => out.push('u'),
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => out.push('i'),
            'ç' | 'Ç' => out.push('c'),
            'ñ' | 'Ñ' => out.push('n'),
            'ß' => out.push_str("ss"),
            _ => {
                for l in c.to_lowercase() {
                    out.push(l);
                }
            }
        }
    }
    out
}

/// Natürliche Sortierung („Datei 2" vor „Datei 10"). Erwartet bereits gefaltete Schlüssel.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    while i < ab.len() && j < bb.len() {
        let (ca, cb) = (ab[i], bb[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            while i < ab.len() && ab[i] == b'0' {
                i += 1;
            }
            while j < bb.len() && bb[j] == b'0' {
                j += 1;
            }
            let (ni, nj) = (i, j);
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let (la, lb) = (i - ni, j - nj);
            if la != lb {
                return la.cmp(&lb);
            }
            match ab[ni..i].cmp(&bb[nj..j]) {
                Ordering::Equal => continue,
                other => return other,
            }
        }
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1;
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}

/// „1,9 MB" – kompakte Größe; Dezimalkomma im Deutschen, Punkt im Englischen.
pub fn fmt_size(n: u64) -> String {
    fmt_size_in(n, crate::lang::english())
}

fn fmt_size_in(n: u64, english: bool) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let s = if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    format!("{} {}", if english { s } else { s.replace('.', ",") }, UNITS[u])
}

/// „1.234.567" – Tausenderpunkte (im Englischen Kommas).
pub fn fmt_thousands(n: u64) -> String {
    fmt_thousands_in(n, crate::lang::english())
}

fn fmt_thousands_in(n: u64, english: bool) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (digits.len() - idx) % 3 == 0 {
            out.push(if english { ',' } else { '.' });
        }
        out.push(ch);
    }
    out
}

pub fn system_time_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

/// „Heute, 12:28" / „Gestern, 09:00" / „25.07.2026, 09:00"
pub fn fmt_time(secs: i64, today: chrono::NaiveDate) -> String {
    let Some(utc) = chrono::DateTime::from_timestamp(secs, 0) else {
        return String::new();
    };
    let local = utc.with_timezone(&chrono::Local);
    let day = local.date_naive();
    let clock = local.format("%H:%M");
    if day == today {
        crate::tf!("Heute, {}", "Today, {}", clock)
    } else if Some(day) == today.pred_opt() {
        crate::tf!("Gestern, {}", "Yesterday, {}", clock)
    } else if crate::lang::english() {
        // Im Englischen die eindeutige Schreibweise Jahr-Monat-Tag
        local.format("%Y-%m-%d, %H:%M").to_string()
    } else {
        local.format("%d.%m.%Y, %H:%M").to_string()
    }
}

/// Platzhalter-Vergleich mit `*` und `?`, beide Seiten bereits gefaltet.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Filter: enthält der Text Platzhalter, wird er als Muster behandelt, sonst als Teilstring.
pub struct NameFilter {
    needle: String,
    wildcard: bool,
}

impl NameFilter {
    pub fn new(query: &str) -> Self {
        let needle = fold_key(query.trim());
        let wildcard = needle.contains('*') || needle.contains('?');
        Self { needle, wildcard }
    }

    pub fn matches(&self, key: &str) -> bool {
        if self.needle.is_empty() {
            true
        } else if self.wildcard {
            wildcard_match(&self.needle, key)
        } else {
            key.contains(&self.needle)
        }
    }
}

/// Teilt „name.ext" in („name", "ext"). Punkt-Dateien wie „.gitignore" haben keine Endung.
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(pos) if pos > 0 && pos + 1 < name.len() => (&name[..pos], &name[pos + 1..]),
        _ => (name, ""),
    }
}

/// Zeichen, die Windows in Dateinamen nicht erlaubt.
pub fn invalid_name_reason(name: &str) -> Option<&'static str> {
    use crate::lang::t;
    if name.is_empty() {
        return Some(t("Der Name ist leer", "The name is empty"));
    }
    if name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32) {
        return Some(t("Enthält ein unzulässiges Zeichen ( \\ / : * ? \" < > | )", "Contains a character that is not allowed ( \\ / : * ? \" < > | )"));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Some(t("Darf nicht mit Leerzeichen oder Punkt enden", "Must not end with a space or a dot"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["datei 10", "datei 2", "datei 1", "abc", "datei 02"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v[0], "abc");
        assert_eq!(v[1], "datei 1");
        assert_eq!(v[4], "datei 10");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.png", "bild.png"));
        assert!(wildcard_match("b?ld*", "bild.png"));
        assert!(!wildcard_match("*.jpg", "bild.png"));
        assert!(wildcard_match("*", ""));
    }

    #[test]
    fn sizes() {
        assert_eq!(fmt_size_in(0, false), "0 B");
        assert_eq!(fmt_size_in(1536, false), "1,50 KB");
        assert_eq!(fmt_size_in(1536, true), "1.50 KB");
        assert_eq!(fmt_thousands_in(1234567, false), "1.234.567");
        assert_eq!(fmt_thousands_in(1234567, true), "1,234,567");
    }

    #[test]
    fn ext_split() {
        assert_eq!(split_ext("a.tar.gz"), ("a.tar", "gz"));
        assert_eq!(split_ext(".gitignore"), (".gitignore", ""));
        assert_eq!(split_ext("ohne"), ("ohne", ""));
    }
}
