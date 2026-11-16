// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Computer names the organization's directory accepts.
//!
//! Active Directory computer accounts use the `NetBIOS` name, which is at most
//! 15 characters. Rabita also keeps to DNS host-name rules so the name works
//! everywhere: letters, digits and hyphens, not starting or ending with a
//! hyphen, and not only digits.

/// Longest computer name Active Directory accepts.
pub const MAX_LEN: usize = 15;

/// The first label of a host name (`pc1.example.org` -> `pc1`).
#[must_use]
pub fn short_name(hostname: &str) -> &str {
    hostname.trim().split('.').next().unwrap_or("")
}

/// Why `name` can't be used as a computer name, in plain words, or `None`.
#[must_use]
pub fn name_problem(name: &str) -> Option<String> {
    let n = name.trim();
    let len = n.chars().count();
    if n.is_empty() {
        return Some("The computer name is empty.".to_owned());
    }
    if len > MAX_LEN {
        return Some(format!(
            "“{n}” is {len} characters long. Computer names in your organization can have at most {MAX_LEN}."
        ));
    }
    if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some(format!(
            "“{n}” contains characters a computer name can't use. Use only letters, numbers and hyphens."
        ));
    }
    if n.starts_with('-') || n.ends_with('-') {
        return Some(format!("“{n}” can't start or end with a hyphen."));
    }
    if n.chars().all(|c| c.is_ascii_digit()) {
        return Some(format!("“{n}” can't be only numbers."));
    }
    None
}

/// A valid name close to `name`. A trailing number (`-01`) is kept, so
/// `lab-classroom-pc-01` becomes `lab-classroo-01`, not `lab-classroom-p`,
/// which would clash with its neighbours.
#[must_use]
pub fn suggest_name(name: &str) -> String {
    let mut cleaned = String::new();
    for c in short_name(name).chars() {
        let c = if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        };
        if !(c == '-' && cleaned.ends_with('-')) {
            cleaned.push(c);
        }
    }
    let cleaned = cleaned.trim_matches('-').to_owned();
    if !cleaned.is_empty() && cleaned.chars().all(|c| c.is_ascii_digit()) {
        // Serial-number style names differ at the end, so keep that part.
        return format!(
            "pc-{}",
            &cleaned[cleaned.len().saturating_sub(MAX_LEN - 3)..]
        );
    }
    let out = if cleaned.len() <= MAX_LEN {
        cleaned
    } else {
        let digits = cleaned.len() - cleaned.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        // Keep up to five trailing digits plus the hyphen before them.
        let mut suffix_len = digits.min(5);
        if digits > 0 && cleaned[..cleaned.len() - digits].ends_with('-') {
            suffix_len = digits.min(4) + 1;
        }
        let suffix = &cleaned[cleaned.len() - suffix_len..];
        let keep = MAX_LEN - suffix.len();
        let prefix = cleaned[..keep].trim_end_matches('-');
        format!("{prefix}{suffix}")
    };
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        return "linux-pc".to_owned();
    }
    out
}

/// Updates `/etc/hosts` text for a new host name: the `127.0.1.1` line
/// (Debian's line for the machine's own name) gets the new name in place of
/// the old one; a line is added if there is none.
#[must_use]
pub fn update_hosts(text: &str, old: &str, new: &str) -> String {
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next() != Some("127.0.1.1") {
                return line.to_owned();
            }
            found = true;
            let mut names: Vec<String> = Vec::new();
            for token in parts {
                if token.starts_with('#') {
                    break;
                }
                let renamed = if token.eq_ignore_ascii_case(old) {
                    new.to_owned()
                } else if token.len() > old.len()
                    && token[..old.len()].eq_ignore_ascii_case(old)
                    && token.as_bytes()[old.len()] == b'.'
                {
                    format!("{new}{}", &token[old.len()..])
                } else {
                    token.to_owned()
                };
                if !names.contains(&renamed) {
                    names.push(renamed);
                }
            }
            if !names
                .iter()
                .any(|n| n == new || n.starts_with(&format!("{new}.")))
            {
                names.push(new.to_owned());
            }
            format!("127.0.1.1\t{}", names.join(" "))
        })
        .collect();
    if !found {
        lines.push(format!("127.0.1.1\t{new}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_names() {
        assert_eq!(name_problem("lab-pc-01"), None);
        assert_eq!(name_problem("PC1"), None);
        assert_eq!(name_problem("abcdefghijklmno"), None, "exactly 15");
    }

    #[test]
    fn explains_bad_names() {
        assert_eq!(
            name_problem("lab-classroom-pc-01").as_deref(),
            Some("“lab-classroom-pc-01” is 19 characters long. Computer names in your organization can have at most 15.")
        );
        assert!(name_problem("lab_pc")
            .unwrap()
            .contains("letters, numbers and hyphens"));
        assert!(name_problem("-pc").unwrap().contains("hyphen"));
        assert!(name_problem("12345").unwrap().contains("only numbers"));
        assert!(name_problem(" ").unwrap().contains("empty"));
    }

    #[test]
    fn suggestions_are_valid_and_keep_the_number() {
        for (input, expected) in [
            ("lab-classroom-pc-01", "lab-classroo-01"),
            ("Teachers_Laptop_Room12", "teachers-lapt12"),
            ("science-lab-computer", "science-lab-com"),
            ("pc1.lab.quietbyte.test", "pc1"),
            ("ubuntu-desktop-2024", "ubuntu-des-2024"),
            ("12345678901234567", "pc-678901234567"),
            ("2024", "pc-2024"),
            ("___", "linux-pc"),
            ("lab-pc-01", "lab-pc-01"),
        ] {
            let got = suggest_name(input);
            assert_eq!(got, expected, "for {input}");
            assert_eq!(name_problem(&got), None, "{got} must be valid");
        }
    }

    #[test]
    fn short_name_takes_the_first_label() {
        assert_eq!(short_name("pc1.example.org"), "pc1");
        assert_eq!(short_name("pc1"), "pc1");
    }

    #[test]
    fn hosts_file_follows_the_rename() {
        let ubuntu = "127.0.0.1\tlocalhost\n127.0.1.1\tlab-classroom-pc-01\n\n::1 ip6-localhost ip6-loopback\n";
        assert_eq!(
            update_hosts(ubuntu, "lab-classroom-pc-01", "lab-classroo-01"),
            "127.0.0.1\tlocalhost\n127.0.1.1\tlab-classroo-01\n\n::1 ip6-localhost ip6-loopback\n"
        );
        let fqdn = "127.0.1.1 old.lab.test old # comment\n";
        assert_eq!(
            update_hosts(fqdn, "old", "new"),
            "127.0.1.1\tnew.lab.test new\n"
        );
        assert_eq!(
            update_hosts("127.0.0.1 localhost\n", "old", "new"),
            "127.0.0.1 localhost\n127.0.1.1\tnew\n"
        );
    }
}
