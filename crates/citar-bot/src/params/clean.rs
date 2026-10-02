//! `profiles.clean_params` (profiles.py:185-225), with its two laxities fixed (DESIGN.md
//! P2.3.2): an `int` must be integral (`2`, `2.0` and `"2"` are 2; `2.5` is refused), and the
//! names in an `order` or `list` must be the spec's options, a preset, `"default"` or `null`.
//!
//! The stub of package 2-00a cleans what needs no schema: `idle` takes no parameters and ignores
//! any, as Python did, and no overrides at all are none for any version. Package 2-01a cleans
//! `basic-1`'s against its schema.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Overrides, ParamError};
use crate::versions::{self, VersionId};

/// `overrides` for version `v` (or `basic`), cleaned.
pub(crate) fn clean(v: &str, overrides: &Value) -> Result<Overrides, ParamError> {
    let Some(version) = VersionId::resolve(v) else {
        return Err(ParamError::new(
            None,
            format!("'{v}' is not a bot version (the versions are {}).", versions::names()),
        ));
    };
    let map = match overrides {
        Value::Null => return Ok(Overrides::default()),
        Value::Object(m) => m,
        _ => return Err(ParamError::new(None, "Bot parameters are an object of {key: value}.")),
    };
    match version {
        VersionId::Idle => Ok(Overrides::default()),
        VersionId::Basic1 if map.is_empty() => Ok(Overrides::cleaned(BTreeMap::new())),
        VersionId::Basic1 => Err(ParamError::new(
            None,
            "basic-1's parameters are cleaned against its schema from package 2-01a on.",
        )),
    }
}
