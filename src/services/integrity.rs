//! Definitions carry an HMAC under a machine secret only SYSTEM and Administrators can read,
//! so the host runs only what K3 Up wrote. Permissions show who may change a file now, not
//! who changed it before.
pub const KEY_LENGTH: usize = 32;
#[cfg(windows)]
const KEY_VALUE: &str = "Key";
/// SYSTEM and Administrators only, also for subkeys.
#[cfg(windows)]
const KEY_SDDL: &str = "D:P(A;CI;KA;;;SY)(A;CI;KA;;;BA)";

/// Where the secret for services named with `prefix` is kept under HKEY_LOCAL_MACHINE. Tests
/// use their own prefix, and with it their own key.
pub fn key_path(prefix: &str) -> String {
    if prefix.eq_ignore_ascii_case(super::DEFAULT_SERVICE_PREFIX) {
        r"SOFTWARE\K3 Up\Integrity".into()
    } else {
        format!(r"SOFTWARE\K3 Up Test\{prefix}")
    }
}

/// What is signed: the workload name in lower case, as Windows matches file and service
/// names, a zero byte, and the definition exactly as written.
pub fn message(name: &str, definition: &[u8]) -> Vec<u8> {
    let mut message = name.to_lowercase().into_bytes();
    message.push(0);
    message.extend_from_slice(definition);
    message
}

/// Compares every byte whatever differs first, so the time taken says nothing about where.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |found, (x, y)| found | (x ^ y)) == 0
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPlan {
    Keep,
    Create,
}

/// A new key would make every existing signature fail, and the services that run those
/// workloads would refuse them at their next start, so one is made only while nothing is
/// signed.
pub fn plan_key(prefix: &str, stored: Option<&[u8]>, signed: usize) -> anyhow::Result<KeyPlan> {
    if stored.is_some_and(|key| key.len() == KEY_LENGTH) {
        return Ok(KeyPlan::Keep);
    }
    if signed == 0 {
        return Ok(KeyPlan::Create);
    }
    let definitions = if signed == 1 {
        "1 workload definition is".to_string()
    } else {
        format!("{signed} workload definitions are")
    };
    anyhow::bail!(
        r"The signing key in HKLM\{} is missing or damaged, but {definitions} signed with it. Restore the key from a backup, or remove those workloads with `k3up remove --stop NAME` and re-apply them from a trusted manifest after `k3up services enable`.",
        key_path(prefix)
    )
}

/// The `.sig` files in `workloads`, whatever the case of their extension.
pub fn signed_definitions(workloads: &std::path::Path) -> anyhow::Result<usize> {
    use anyhow::Context;
    let entries = match std::fs::read_dir(workloads) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        entries => entries.with_context(|| format!("Read {}", workloads.display()))?,
    };
    let mut signed = 0;
    for entry in entries {
        let path = entry
            .with_context(|| format!("Read {}", workloads.display()))?
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sig"))
        {
            signed += 1;
        }
    }
    Ok(signed)
}

/// Refuses, changing nothing, when the secret for `prefix` is missing or damaged while
/// definitions in `workloads` are signed.
#[cfg(windows)]
pub fn check_key(prefix: &str, workloads: &std::path::Path) -> anyhow::Result<()> {
    let stored = crate::win32::registry_binary(
        crate::win32::Hive::LocalMachine,
        &key_path(prefix),
        KEY_VALUE,
    );
    plan_key(prefix, stored.as_deref(), signed_definitions(workloads)?).map(drop)
}

/// Creates the secret for `prefix` when it is missing and nothing in `workloads` is signed,
/// and protects its key either way.
#[cfg(windows)]
pub fn ensure_key(prefix: &str, workloads: &std::path::Path) -> anyhow::Result<()> {
    check_key(prefix, workloads)?;
    crate::win32::protected_registry_binary(
        &key_path(prefix),
        KEY_SDDL,
        KEY_VALUE,
        KEY_LENGTH,
        || {
            plan_key(prefix, None, signed_definitions(workloads)?)?;
            crate::win32::random_bytes(KEY_LENGTH)
        },
    )
    .map(|_| ())
}

#[cfg(windows)]
pub fn load_key(prefix: &str) -> anyhow::Result<Vec<u8>> {
    crate::win32::registry_binary(
        crate::win32::Hive::LocalMachine,
        &key_path(prefix),
        KEY_VALUE,
    )
    .filter(|key| key.len() == KEY_LENGTH)
    .ok_or_else(|| {
        anyhow::anyhow!(
            r"The services mode signing key in HKLM\{} is missing, damaged or unreadable. Run k3up from an elevated terminal. If the key is lost, restore it from a backup, or remove the workloads with `k3up remove --stop NAME` and re-apply them from a trusted manifest after `k3up services enable`.",
            key_path(prefix)
        )
    })
}

/// The signature file's text for `definition`.
#[cfg(windows)]
pub fn sign(key: &[u8], name: &str, definition: &[u8]) -> anyhow::Result<String> {
    Ok(to_hex(&crate::win32::hmac_sha256(
        key,
        &message(name, definition),
    )?))
}

#[cfg(windows)]
pub fn verify(key: &[u8], name: &str, definition: &[u8], signature: &str) -> anyhow::Result<bool> {
    let expected = crate::win32::hmac_sha256(key, &message(name, definition))?;
    Ok(from_hex(signature).is_some_and(|found| same(&found, &expected)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signed_message_binds_the_name_in_lower_case() {
        assert_eq!(message("Web", b"a = 1\n"), b"web\0a = 1\n");
        assert_eq!(message("web", b""), b"web\0");
        assert_ne!(message("ab", b"c"), message("a", b"bc"));
    }

    #[test]
    fn comparison_needs_every_byte_and_the_length_to_match() {
        assert!(same(b"", b""));
        assert!(same(&[1, 2, 3], &[1, 2, 3]));
        assert!(!same(&[1, 2, 3], &[1, 2, 4]));
        assert!(!same(&[0, 2, 3], &[1, 2, 3]));
        assert!(!same(&[1, 2], &[1, 2, 3]));
    }

    #[test]
    fn signatures_are_lower_case_hex() {
        assert_eq!(to_hex(&[0x00, 0xab, 0x7f]), "00ab7f");
        assert_eq!(from_hex("00ab7f\n"), Some(vec![0x00, 0xab, 0x7f]));
        assert_eq!(from_hex("00AB7F"), Some(vec![0x00, 0xab, 0x7f]));
        for broken in ["0", "zz", "0é", "+1"] {
            assert_eq!(from_hex(broken), None, "{broken}");
        }
    }

    #[test]
    fn a_key_is_created_only_while_nothing_is_signed_with_the_old_one() {
        let valid = [7u8; KEY_LENGTH];
        let short = [7u8; KEY_LENGTH - 1];
        assert_eq!(plan_key("K3Up_", None, 0).unwrap(), KeyPlan::Create);
        assert_eq!(plan_key("K3Up_", Some(&short), 0).unwrap(), KeyPlan::Create);
        assert_eq!(plan_key("K3Up_", Some(&valid), 0).unwrap(), KeyPlan::Keep);
        assert_eq!(plan_key("K3Up_", Some(&valid), 3).unwrap(), KeyPlan::Keep);
        for stored in [None, Some(&short[..]), Some(&[][..])] {
            assert_eq!(
                plan_key("K3Up_", stored, 2).unwrap_err().to_string(),
                r"The signing key in HKLM\SOFTWARE\K3 Up\Integrity is missing or damaged, but 2 workload definitions are signed with it. Restore the key from a backup, or remove those workloads with `k3up remove --stop NAME` and re-apply them from a trusted manifest after `k3up services enable`."
            );
        }
        let one = plan_key("K3Up_", None, 1).unwrap_err().to_string();
        assert!(
            one.contains(", but 1 workload definition is signed with it."),
            "{one}"
        );
    }

    #[test]
    fn signatures_are_counted_whatever_the_case_of_their_extension() {
        let dir = tempfile::tempdir().unwrap();
        let workloads = dir.path().join("workloads");
        assert_eq!(signed_definitions(&workloads).unwrap(), 0);
        std::fs::create_dir(&workloads).unwrap();
        for name in ["web.toml", "web.sig", "api.SIG", "notes.txt", "sig"] {
            std::fs::write(workloads.join(name), "").unwrap();
        }
        assert_eq!(signed_definitions(&workloads).unwrap(), 2);
    }

    #[test]
    fn test_prefixes_keep_their_own_key() {
        assert_eq!(
            key_path(super::super::DEFAULT_SERVICE_PREFIX),
            r"SOFTWARE\K3 Up\Integrity"
        );
        assert_eq!(key_path("k3up_"), r"SOFTWARE\K3 Up\Integrity");
        assert_eq!(key_path("K3UpTest1_"), r"SOFTWARE\K3 Up Test\K3UpTest1_");
    }

    #[cfg(windows)]
    #[test]
    fn hmac_matches_rfc_4231() {
        let mac = crate::win32::hmac_sha256(b"Jefe", b"what do ya want for nothing?").unwrap();
        assert_eq!(
            to_hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let key = [7u8; KEY_LENGTH];
        let signature = sign(&key, "Web", b"a = 1\n").unwrap();
        assert!(verify(&key, "web", b"a = 1\n", &signature).unwrap());
        assert!(!verify(&key, "web", b"a = 2\n", &signature).unwrap());
        assert!(!verify(&key, "api", b"a = 1\n", &signature).unwrap());
        assert!(!verify(&[8u8; KEY_LENGTH], "web", b"a = 1\n", &signature).unwrap());
        assert!(!verify(&key, "web", b"a = 1\n", "").unwrap());
    }
}
