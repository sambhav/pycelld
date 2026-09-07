//! Optional spelling normalization for S3 conditional-write tokens.
//!
//! Preserve is the default: ETags are opaque. Compatibility modes only alter
//! the surrounding quotes, never the value, and never turn a weak validator,
//! list, or wildcard into an ownership precondition.

use anyhow::{bail, ensure};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum S3EtagMode {
    #[default]
    Preserve,
    Quoted,
    Unquoted,
}

impl S3EtagMode {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(crate::env_vars::value("CELLD_S3_ETAG_MODE")?.as_deref())
    }

    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value {
            None | Some("preserve") => Ok(Self::Preserve),
            Some("quoted") => Ok(Self::Quoted),
            Some("unquoted") => Ok(Self::Unquoted),
            Some(value) => {
                bail!("CELLD_S3_ETAG_MODE must be preserve, quoted, or unquoted, not {value:?}")
            }
        }
    }

    pub fn token(self, token: String) -> anyhow::Result<String> {
        if self == Self::Preserve {
            return Ok(token);
        }
        let value = token
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(&token);
        // A narrow compatibility grammar: one nonempty ASCII opaque value.
        // In particular, stripping quotes from "*" or a comma-containing
        // value would change the meaning of the If-Match header.
        ensure!(
            !value.is_empty()
                && value != "*"
                && !value.starts_with("W/")
                && value
                    .bytes()
                    .all(|c| c.is_ascii_graphic() && c != b'"' && c != b','),
            "S3 ETag cannot be safely normalized: expected one nonempty strong token"
        );
        Ok(match self {
            Self::Quoted => format!("\"{value}\""),
            Self::Unquoted => value.to_string(),
            Self::Preserve => unreachable!(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::S3EtagMode::{self, *};

    #[test]
    fn mode_is_explicit_and_strict() {
        assert_eq!(S3EtagMode::parse(None).unwrap(), Preserve);
        for (value, mode) in [
            ("preserve", Preserve),
            ("quoted", Quoted),
            ("unquoted", Unquoted),
        ] {
            assert_eq!(S3EtagMode::parse(Some(value)).unwrap(), mode);
        }
        for value in ["", "true", "1", "ceph", "Quoted", " unquoted"] {
            assert!(S3EtagMode::parse(Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn default_preserves_the_exact_opaque_value() {
        for token in ["abc", "\"abc\"", "multi-part-12", "provider-specific:ABC"] {
            assert_eq!(Preserve.token(token.into()).unwrap(), token);
        }
    }

    #[test]
    fn normalizes_put_get_and_head_spellings_without_changing_the_value() {
        for value in ["abc123", "9ab01-17", "ABC:123_+/="] {
            let quoted = format!("\"{value}\"");
            for input in [value, quoted.as_str()] {
                assert_eq!(Quoted.token(input.into()).unwrap(), quoted);
                assert_eq!(Unquoted.token(input.into()).unwrap(), value);
            }
        }
    }

    #[test]
    fn refuses_tokens_that_could_weaken_a_precondition() {
        for token in [
            "",
            "\"\"",
            "*",
            "\"*\"",
            "W/\"abc\"",
            "W/abc",
            "\"W/abc\"",
            "abc,def",
            "\"abc,def\"",
            "\"abc\",\"def\"",
            "\"abc",
            "abc\"",
            "a\"b",
            " abc",
            "abc ",
            "a b",
            "a\tb",
            "a\nb",
            "a\rb",
            "a\0b",
            "é",
        ] {
            for mode in [Quoted, Unquoted] {
                assert!(mode.token(token.into()).is_err(), "{mode:?}: {token:?}");
            }
        }
    }
}
