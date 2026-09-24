//! The parts of `cargo metadata` the checks read.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    pub workspace_members: Vec<String>,
    pub resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub id: String,
    pub dependencies: Vec<Dependency>,
    pub targets: Vec<Target>,
    /// The package's own features and what each turns on.
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct Dependency {
    /// The package name, even when the dependency is renamed.
    pub name: String,
    pub req: String,
    /// `None` for a normal dependency, else `"dev"` or `"build"`.
    pub kind: Option<String>,
    pub uses_default_features: bool,
    /// The features the dependent turns on.
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Target {
    pub kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Resolve {
    pub nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
pub struct Node {
    pub id: String,
    /// The features resolved for this package across the workspace.
    pub features: Vec<String>,
}

impl Metadata {
    /// Runs `cargo metadata` with every feature on, so a feature that could pull in a dependency
    /// or a `libm` feature is seen even when it is off by default.
    pub fn load(root: &Path) -> Result<Self, String> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let output = Command::new(cargo)
            .args(["metadata", "--format-version", "1", "--all-features", "--manifest-path"])
            .arg(root.join("Cargo.toml"))
            .output()
            .map_err(|e| format!("running cargo metadata: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Self::parse(&output.stdout)
    }

    pub fn parse(json: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(json).map_err(|e| format!("reading cargo metadata: {e}"))
    }

    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn members(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(|p| self.workspace_members.contains(&p.id))
    }

    /// The resolved features of the package with this id.
    pub fn features_of(&self, id: &str) -> Option<&[String]> {
        let node = self.resolve.as_ref()?.nodes.iter().find(|n| n.id == id)?;
        Some(&node.features)
    }
}
