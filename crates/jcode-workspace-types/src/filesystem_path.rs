//! Lossless paths for closeout entries. Ordinary Unicode paths keep their
//! existing string representation. Unix-only opaque paths carry native bytes.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnixBytes {
    unix_bytes: Vec<u8>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Wire {
    Text(String),
    Bytes(UnixBytes),
}

pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    if let Some(text) = path.to_str() {
        return serializer.serialize_str(text);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        UnixBytes {
            unix_bytes: path.as_os_str().as_bytes().to_vec(),
        }
        .serialize(serializer)
    }
    #[cfg(not(unix))]
    {
        Err(serde::ser::Error::custom(
            "This platform cannot serialize an opaque Unix path",
        ))
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    match Wire::deserialize(deserializer)? {
        Wire::Text(text) => {
            if text.contains('\0') {
                return Err(serde::de::Error::custom(
                    "Filesystem paths cannot contain NUL",
                ));
            }
            Ok(text.into())
        }
        Wire::Bytes(bytes) => {
            if bytes.unix_bytes.contains(&0) {
                return Err(serde::de::Error::custom(
                    "Filesystem paths cannot contain NUL",
                ));
            }
            if std::str::from_utf8(&bytes.unix_bytes).is_ok() {
                return Err(serde::de::Error::custom(
                    "Unicode filesystem paths must use their string representation",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                Ok(std::ffi::OsString::from_vec(bytes.unix_bytes).into())
            }
            #[cfg(not(unix))]
            {
                Err(serde::de::Error::custom(
                    "Opaque Unix paths are unsupported on this platform",
                ))
            }
        }
    }
}

pub mod optional {
    use super::*;
    struct Encoded<'a>(&'a Path);
    impl Serialize for Encoded<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            super::serialize(self.0, serializer)
        }
    }
    #[derive(Deserialize)]
    struct Decoded(#[serde(with = "super")] PathBuf);
    pub fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match path {
            Some(path) => serializer.serialize_some(&Encoded(path)),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Ok(Option::<Decoded>::deserialize(deserializer)?.map(|path| path.0))
    }
}
